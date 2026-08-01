use crate::builder::{
    JunctionKey, JunctionPolicy, SegmentDraft, TurnRestrictionDraft, TurnRestrictionRule,
};
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, CrossingControl, CrossingKind, EdgeTravel, GeometryClaim, Provenance, Terrain,
    TrailMarking, TrailStanding, WayKind, WayRealm,
};
use crate::overlay::ContextOverlay;
use crate::{Result, TrailgenError};
use osmpbfreader::{
    Node as PbfNode, OsmId, OsmObj, OsmPbfReader, Relation as PbfRelation, Tags as PbfTags,
    Way as PbfWay, WayId,
};
use std::collections::BTreeMap;
use std::io::{Read, Seek};

struct OsmDraft {
    segment: SegmentDraft,
    junctions: Vec<JunctionKey>,
}

#[derive(Clone, Copy)]
struct XmlNode {
    coord: Coord,
    crossing_control: CrossingControl,
}

pub fn network_from_str(s: &str) -> Result<Vec<SegmentDraft>> {
    let doc = roxmltree::Document::parse(s)
        .map_err(|error| TrailgenError::Xml(format!("parse OSM XML: {error}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "osm" {
        return Err(TrailgenError::UnsupportedFormat(
            "OSM XML network must have <osm> root".to_owned(),
        ));
    }
    let nodes = root
        .children()
        .filter(|node| node.has_tag_name("node"))
        .map(parse_node)
        .collect::<Result<BTreeMap<_, _>>>()?;
    let relations = xml_relation_evidence(root, &nodes)?;
    let drafts = root
        .children()
        .filter(|node| node.has_tag_name("way"))
        .filter_map(|way| draft_from_xml_way(way, &nodes, &relations).transpose())
        .collect::<Result<Vec<_>>>()?;
    let mut drafts = contract_osm_drafts(drafts);
    seat_turn_restrictions(&mut drafts, relations.turn_restrictions);
    Ok(drafts)
}

pub fn network_from_pbf_reader<R: Read + Seek>(reader: R) -> Result<Vec<SegmentDraft>> {
    let mut pbf = OsmPbfReader::new(reader);
    let objects = pbf
        .get_objs_and_deps(pbf_network_object)
        .map_err(|error| TrailgenError::InvalidData(format!("parse OSM PBF: {error}")))?;
    let relations = pbf_relation_evidence(&objects);
    let drafts = objects
        .values()
        .filter_map(|object| match object {
            OsmObj::Way(way) => draft_from_pbf_way(way, &objects, &relations).transpose(),
            OsmObj::Node(_) | OsmObj::Relation(_) => None,
        })
        .collect::<Result<Vec<_>>>()?;
    let mut drafts = contract_osm_drafts(drafts);
    seat_turn_restrictions(&mut drafts, relations.turn_restrictions);
    Ok(drafts)
}

pub fn context_overlays_from_str(s: &str) -> Result<Vec<ContextOverlay>> {
    let doc = roxmltree::Document::parse(s)
        .map_err(|error| TrailgenError::Xml(format!("parse OSM XML: {error}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "osm" {
        return Err(TrailgenError::UnsupportedFormat(
            "OSM XML context layer must have <osm> root".to_owned(),
        ));
    }
    let nodes = root
        .children()
        .filter(|node| node.has_tag_name("node"))
        .map(parse_node)
        .collect::<Result<BTreeMap<_, _>>>()?;
    root.children()
        .filter(|node| node.has_tag_name("way"))
        .filter_map(|way| context_from_xml_way(way, &nodes).transpose())
        .collect()
}

pub fn context_overlays_from_pbf_reader<R: Read + Seek>(reader: R) -> Result<Vec<ContextOverlay>> {
    let mut pbf = OsmPbfReader::new(reader);
    let objects = pbf
        .get_objs_and_deps(pbf_context_way)
        .map_err(|error| TrailgenError::InvalidData(format!("parse OSM PBF: {error}")))?;
    objects
        .values()
        .filter_map(|object| match object {
            OsmObj::Way(way) => context_from_pbf_way(way, &objects).transpose(),
            OsmObj::Node(_) | OsmObj::Relation(_) => None,
        })
        .collect()
}

fn parse_node(node: roxmltree::Node<'_, '_>) -> Result<(String, XmlNode)> {
    let id = required_attr(node, "id")?.to_owned();
    let lon = required_attr(node, "lon")?
        .parse::<f64>()
        .map_err(|error| TrailgenError::InvalidData(format!("invalid OSM node lon: {error}")))?;
    let lat = required_attr(node, "lat")?
        .parse::<f64>()
        .map_err(|error| TrailgenError::InvalidData(format!("invalid OSM node lat: {error}")))?;
    if !lon.is_finite() || !lat.is_finite() {
        return Err(TrailgenError::InvalidData(
            "OSM node coordinates must be finite".to_owned(),
        ));
    }
    let tags = xml_tags(node);
    let ele = tags
        .get("ele")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite());
    Ok((
        id,
        XmlNode {
            coord: Coord { lon, lat, ele },
            crossing_control: crossing_control_from_tags(&tags, WayKind::Crossing),
        },
    ))
}

fn draft_from_xml_way(
    way: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, XmlNode>,
    relations: &OsmRelationEvidence,
) -> Result<Option<OsmDraft>> {
    let tags = xml_tags(way);
    let id = way_id(way);
    let Some(walkway) = Walkway::from_tags(&tags, relations.route_by_way.contains_key(&id)) else {
        return Ok(None);
    };
    let mut points = Vec::new();
    let mut junctions = Vec::new();
    let mut crossing_control = walkway.crossing_control;
    for nd in way.children().filter(|node| node.has_tag_name("nd")) {
        let reference = required_attr(nd, "ref")?;
        let Some(node) = nodes.get(reference) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM way {id} references missing node {reference}"
            )));
        };
        points.push(node.coord);
        junctions.push(osm_junction(reference));
        if walkway.kind == WayKind::Crossing {
            crossing_control = crossing_control.max(node.crossing_control);
        }
    }
    if points.len() < 2 {
        return Ok(None);
    }
    Ok(Some(OsmDraft {
        junctions,
        segment: SegmentDraft {
            junctions: source_junction_policy(&tags),
            turn_ref: Some(id.clone()),
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(points)?,
            way_kind: walkway.kind,
            realm: walkway.realm,
            geometry_claim: walkway.geometry_claim,
            crossing_control,
            standing: walkway.standing,
            marking: walkway.marking,
            terrain: walkway.terrain,
            terrain_confidence: Some(walkway.terrain_confidence),
            surface: tags.get("surface").cloned(),
            access: walkway.access,
            travel: walkway.travel,
            road_exposure: walkway.road_exposure,
            confidence: relation_boosted_confidence(
                walkway.confidence,
                relations.route_by_way.get(&id).map_or(0, Vec::len),
            ),
            provenance: vec![osm_way_provenance("osm-xml", &id, relations)],
        },
    }))
}

fn context_from_xml_way(
    way: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, XmlNode>,
) -> Result<Option<ContextOverlay>> {
    let tags = xml_tags(way);
    let Some(kind) = context_kind_from_tags(&tags) else {
        return Ok(None);
    };
    let id = way_id(way);
    let mut points = Vec::new();
    for nd in way.children().filter(|node| node.has_tag_name("nd")) {
        let reference = required_attr(nd, "ref")?;
        let Some(node) = nodes.get(reference) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM way {id} references missing node {reference}"
            )));
        };
        points.push(node.coord);
    }
    context_overlay(kind, &tags, id, points)
}

fn pbf_walkable_way(object: &OsmObj) -> bool {
    object
        .way()
        .is_some_and(|way| Walkway::from_tags(&pbf_tags(&way.tags), false).is_some())
}

fn pbf_network_object(object: &OsmObj) -> bool {
    pbf_walkable_way(object)
        || pbf_hiking_route_relation(object)
        || pbf_turn_restriction_relation(object)
}

fn pbf_hiking_route_relation(object: &OsmObj) -> bool {
    object
        .relation()
        .is_some_and(|relation| route_relation_from_tags(&pbf_tags(&relation.tags)).is_some())
}

fn pbf_turn_restriction_relation(object: &OsmObj) -> bool {
    object
        .relation()
        .is_some_and(|relation| turn_restriction_from_tags(&pbf_tags(&relation.tags)).is_some())
}

fn pbf_context_way(object: &OsmObj) -> bool {
    object
        .way()
        .is_some_and(|way| context_kind_from_tags(&pbf_tags(&way.tags)).is_some())
}

fn draft_from_pbf_way(
    way: &PbfWay,
    objects: &BTreeMap<OsmId, OsmObj>,
    relations: &OsmRelationEvidence,
) -> Result<Option<OsmDraft>> {
    let tags = pbf_tags(&way.tags);
    let id = way.id.0.to_string();
    let Some(walkway) = Walkway::from_tags(&tags, relations.route_by_way.contains_key(&id)) else {
        return Ok(None);
    };
    let mut points = Vec::new();
    let mut junctions = Vec::new();
    let mut crossing_control = walkway.crossing_control;
    for id in &way.nodes {
        let Some(OsmObj::Node(node)) = objects.get(&OsmId::Node(*id)) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM PBF way {} references missing node {}",
                way.id.0, id.0
            )));
        };
        points.push(pbf_coord(node));
        junctions.push(osm_junction(&id.0.to_string()));
        if walkway.kind == WayKind::Crossing {
            crossing_control = crossing_control.max(crossing_control_from_tags(
                &pbf_tags(&node.tags),
                WayKind::Crossing,
            ));
        }
    }
    if points.len() < 2 {
        return Ok(None);
    }
    Ok(Some(OsmDraft {
        junctions,
        segment: SegmentDraft {
            junctions: source_junction_policy(&tags),
            turn_ref: Some(id),
            junction_keys: None,
            turn_restrictions: Vec::new(),
            geometry: LineString::new(points)?,
            way_kind: walkway.kind,
            realm: walkway.realm,
            geometry_claim: walkway.geometry_claim,
            crossing_control,
            standing: walkway.standing,
            marking: walkway.marking,
            terrain: walkway.terrain,
            terrain_confidence: Some(walkway.terrain_confidence),
            surface: tags.get("surface").cloned(),
            access: walkway.access,
            travel: walkway.travel,
            road_exposure: walkway.road_exposure,
            confidence: relation_boosted_confidence(
                walkway.confidence,
                relations
                    .route_by_way
                    .get(&way.id.0.to_string())
                    .map_or(0, Vec::len),
            ),
            provenance: vec![osm_way_provenance(
                "osm-pbf",
                &way.id.0.to_string(),
                relations,
            )],
        },
    }))
}

fn context_from_pbf_way(
    way: &PbfWay,
    objects: &BTreeMap<OsmId, OsmObj>,
) -> Result<Option<ContextOverlay>> {
    let tags = pbf_tags(&way.tags);
    let Some(kind) = context_kind_from_tags(&tags) else {
        return Ok(None);
    };
    let mut points = Vec::new();
    for id in &way.nodes {
        let Some(OsmObj::Node(node)) = objects.get(&OsmId::Node(*id)) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM PBF way {} references missing node {}",
                way.id.0, id.0
            )));
        };
        points.push(pbf_coord(node));
    }
    context_overlay(kind, &tags, way.id.0.to_string(), points)
}

fn pbf_coord(node: &PbfNode) -> Coord {
    let ele = pbf_tags(&node.tags)
        .get("ele")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite());
    Coord {
        lon: node.lon(),
        lat: node.lat(),
        ele,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RouteRelationEvidence {
    id: String,
    label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TurnRestrictionEvidence {
    id: String,
    restriction: String,
    role: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct OsmRelationEvidence {
    route_by_way: BTreeMap<String, Vec<RouteRelationEvidence>>,
    turn_restriction_by_way: BTreeMap<String, Vec<TurnRestrictionEvidence>>,
    turn_restrictions: Vec<TurnRestrictionDraft>,
}

fn seat_turn_restrictions(drafts: &mut [SegmentDraft], restrictions: Vec<TurnRestrictionDraft>) {
    if let Some(carrier) = drafts.first_mut() {
        carrier.turn_restrictions = restrictions;
    }
}

fn contract_osm_drafts(drafts: Vec<OsmDraft>) -> Vec<SegmentDraft> {
    let mut occurrences = BTreeMap::<JunctionKey, usize>::new();
    for junction in drafts.iter().flat_map(|draft| &draft.junctions) {
        *occurrences.entry(junction.clone()).or_default() += 1;
    }
    drafts
        .into_iter()
        .flat_map(|draft| {
            let segment = draft.segment;
            let last = segment.geometry.points.len() - 1;
            let mut cuts = vec![0];
            cuts.extend((1..last).filter(|index| {
                occurrences
                    .get(&draft.junctions[*index])
                    .is_some_and(|count| *count > 1)
            }));
            cuts.push(last);
            cuts.windows(2)
                .map(|cut| SegmentDraft {
                    junctions: if segment.junctions == JunctionPolicy::GradeSeparatedEndpoints {
                        JunctionPolicy::GradeSeparatedEndpoints
                    } else {
                        JunctionPolicy::ExplicitEndpoints
                    },
                    turn_ref: segment.turn_ref.clone(),
                    junction_keys: Some([
                        draft.junctions[cut[0]].clone(),
                        draft.junctions[cut[1]].clone(),
                    ]),
                    turn_restrictions: Vec::new(),
                    geometry: LineString::unchecked(
                        segment.geometry.points[cut[0]..=cut[1]].to_vec(),
                    ),
                    way_kind: segment.way_kind,
                    realm: segment.realm,
                    geometry_claim: segment.geometry_claim,
                    crossing_control: segment.crossing_control,
                    standing: segment.standing,
                    marking: segment.marking,
                    terrain: segment.terrain,
                    terrain_confidence: segment.terrain_confidence,
                    surface: segment.surface.clone(),
                    access: segment.access,
                    travel: segment.travel,
                    road_exposure: segment.road_exposure,
                    confidence: segment.confidence,
                    provenance: segment.provenance.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn osm_junction(id: &str) -> JunctionKey {
    JunctionKey(format!("osm:{id}"))
}

fn xml_relation_evidence(
    root: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, XmlNode>,
) -> Result<OsmRelationEvidence> {
    let mut evidence = OsmRelationEvidence::default();
    for relation in root.children().filter(|node| node.has_tag_name("relation")) {
        let tags = xml_tags(relation);
        let id = relation_id(relation);
        let route = route_relation_from_tags(&tags).map(|label| RouteRelationEvidence {
            id: id.clone(),
            label,
        });
        let turn_restriction = turn_restriction_from_tags(&tags);
        let mut from_way = None::<String>;
        let mut to_way = None::<String>;
        let mut via = None::<Coord>;
        let mut via_key = None::<JunctionKey>;
        for member in relation
            .children()
            .filter(|node| node.has_tag_name("member"))
        {
            let member_type = required_attr(member, "type")?;
            let role = required_attr(member, "role")?;
            if member_type == "node" && role == "via" {
                let reference = required_attr(member, "ref")?;
                if let Some(node) = nodes.get(reference) {
                    via = Some(node.coord);
                    via_key = Some(osm_junction(reference));
                }
                continue;
            }
            if member_type != "way" {
                continue;
            }
            let way = required_attr(member, "ref")?.to_owned();
            match role {
                "from" => from_way = Some(way.clone()),
                "to" => to_way = Some(way.clone()),
                _ => {}
            }
            if let Some(route) = &route {
                evidence
                    .route_by_way
                    .entry(way.clone())
                    .or_default()
                    .push(route.clone());
            }
            if let Some(restriction) = &turn_restriction
                && matches!(role, "from" | "to")
            {
                evidence
                    .turn_restriction_by_way
                    .entry(way)
                    .or_default()
                    .push(TurnRestrictionEvidence {
                        id: id.clone(),
                        restriction: restriction.clone(),
                        role: role.to_owned(),
                    });
            }
        }
        if let (Some(restriction), Some(from), Some(via), Some(to)) =
            (turn_restriction, from_way, via, to_way)
        {
            evidence.turn_restrictions.push(turn_restriction_draft(
                "osm-xml",
                &id,
                &restriction,
                from,
                via,
                via_key,
                to,
            ));
        }
    }
    Ok(evidence)
}

fn pbf_relation_evidence(objects: &BTreeMap<OsmId, OsmObj>) -> OsmRelationEvidence {
    let mut evidence = OsmRelationEvidence::default();
    for relation in objects.values().filter_map(OsmObj::relation) {
        let route = pbf_route_relation_evidence(relation);
        let turn_restriction = pbf_turn_restriction_evidence(relation);
        let mut from_way = None::<String>;
        let mut to_way = None::<String>;
        let mut via = None::<Coord>;
        let mut via_key = None::<JunctionKey>;
        for reference in &relation.refs {
            if let OsmId::Node(node_id) = reference.member
                && reference.role.as_str() == "via"
                && let Some(OsmObj::Node(node)) = objects.get(&OsmId::Node(node_id))
            {
                via = Some(pbf_coord(node));
                via_key = Some(osm_junction(&node_id.0.to_string()));
            }
            if let OsmId::Way(WayId(id)) = reference.member {
                match reference.role.as_str() {
                    "from" => from_way = Some(id.to_string()),
                    "to" => to_way = Some(id.to_string()),
                    _ => {}
                }
                if let Some(route) = &route {
                    evidence
                        .route_by_way
                        .entry(id.to_string())
                        .or_default()
                        .push(route.clone());
                }
                if let Some(restriction) = &turn_restriction
                    && matches!(reference.role.as_str(), "from" | "to")
                {
                    evidence
                        .turn_restriction_by_way
                        .entry(id.to_string())
                        .or_default()
                        .push(TurnRestrictionEvidence {
                            id: restriction.id.clone(),
                            restriction: restriction.restriction.clone(),
                            role: reference.role.to_string(),
                        });
                }
            }
        }
        if let (Some(restriction), Some(from), Some(via), Some(to)) =
            (turn_restriction, from_way, via, to_way)
        {
            evidence.turn_restrictions.push(turn_restriction_draft(
                "osm-pbf",
                &restriction.id,
                &restriction.restriction,
                from,
                via,
                via_key,
                to,
            ));
        }
    }
    evidence
}

fn pbf_route_relation_evidence(relation: &PbfRelation) -> Option<RouteRelationEvidence> {
    route_relation_from_tags(&pbf_tags(&relation.tags)).map(|label| RouteRelationEvidence {
        id: relation.id.0.to_string(),
        label,
    })
}

fn route_relation_from_tags(tags: &BTreeMap<String, String>) -> Option<String> {
    if tags.get("type").is_none_or(|value| value != "route") {
        return None;
    }
    let route = tags.get("route")?;
    if !matches!(route.as_str(), "hiking" | "foot" | "walking") {
        return None;
    }
    Some(
        tags.get("name")
            .or_else(|| tags.get("ref"))
            .map_or_else(|| format!("{route} route"), Clone::clone),
    )
}

fn pbf_turn_restriction_evidence(relation: &PbfRelation) -> Option<TurnRestrictionEvidence> {
    turn_restriction_from_tags(&pbf_tags(&relation.tags)).map(|restriction| {
        TurnRestrictionEvidence {
            id: relation.id.0.to_string(),
            restriction,
            role: String::new(),
        }
    })
}

fn turn_restriction_draft(
    source: &str,
    relation_id: &str,
    restriction: &str,
    from: String,
    via: Coord,
    via_key: Option<JunctionKey>,
    to: String,
) -> TurnRestrictionDraft {
    let rule = if restriction.starts_with("only_") {
        TurnRestrictionRule::Only
    } else {
        TurnRestrictionRule::No
    };
    TurnRestrictionDraft {
        from,
        via,
        via_key,
        to,
        rule,
        provenance: Provenance {
            source: source.to_owned(),
            layer: Some("turn-restriction".to_owned()),
            source_id: Some(format!("{relation_id}:{restriction}")),
            license: Some("ODbL-1.0".to_owned()),
        },
    }
}

fn turn_restriction_from_tags(tags: &BTreeMap<String, String>) -> Option<String> {
    if tags.get("type").is_none_or(|value| value != "restriction") {
        return None;
    }
    tags.get("restriction:foot")
        .filter(|restriction| !restriction.is_empty())
        .cloned()
}

fn osm_way_provenance(source: &str, way_id: &str, relations: &OsmRelationEvidence) -> Provenance {
    let route_label = relations.route_by_way.get(way_id).map(|relations| {
        relations
            .iter()
            .map(RouteRelationEvidence::slug)
            .collect::<Vec<_>>()
            .join(",")
    });
    let turn_restriction_label =
        relations
            .turn_restriction_by_way
            .get(way_id)
            .map(|restrictions| {
                restrictions
                    .iter()
                    .map(TurnRestrictionEvidence::slug)
                    .collect::<Vec<_>>()
                    .join(",")
            });
    let source_id = match (&route_label, &turn_restriction_label) {
        (None, None) => way_id.to_owned(),
        (routes, restrictions) => {
            let mut id = format!("way {way_id}");
            if let Some(routes) = routes {
                id.push_str("; route relations ");
                id.push_str(routes);
            }
            if let Some(restrictions) = restrictions {
                id.push_str("; turn restrictions ");
                id.push_str(restrictions);
            }
            id
        }
    };
    let layer = relation_layer(route_label.is_some(), turn_restriction_label.is_some());
    Provenance {
        source: source.to_owned(),
        layer: Some(layer.to_owned()),
        source_id: Some(source_id),
        license: Some("ODbL-1.0".to_owned()),
    }
}

const fn relation_layer(has_route: bool, has_turn_restriction: bool) -> &'static str {
    match (has_route, has_turn_restriction) {
        (false, false) => "way",
        (true, false) => "way+route-relation",
        (false, true) => "way+turn-restriction",
        (true, true) => "way+route-relation+turn-restriction",
    }
}

impl RouteRelationEvidence {
    fn slug(&self) -> String {
        format!("{}:{}", self.id, self.label)
    }
}

impl TurnRestrictionEvidence {
    fn slug(&self) -> String {
        format!("{}:{}:{}", self.id, self.role, self.restriction)
    }
}

const fn relation_boosted_confidence(confidence: f64, route_relation_count: usize) -> f64 {
    if route_relation_count == 0 {
        confidence
    } else {
        confidence.max(0.82)
    }
}

fn context_overlay(
    kind: CrossingKind,
    tags: &BTreeMap<String, String>,
    id: String,
    points: Vec<Coord>,
) -> Result<Option<ContextOverlay>> {
    if points.len() < 2 {
        return Ok(None);
    }
    let name = tags
        .get("name")
        .or_else(|| tags.get("ref"))
        .cloned()
        .unwrap_or_else(|| format!("osm-way-{id}"));
    Ok(Some(ContextOverlay {
        name,
        kind,
        confidence: 0.78,
        provenance: Provenance {
            source: osm_context_source(kind).to_owned(),
            layer: Some("way".to_owned()),
            source_id: Some(id),
            license: Some("ODbL-1.0".to_owned()),
        },
        geometry: LineString::new(points)?,
    }))
}

fn context_kind_from_tags(tags: &BTreeMap<String, String>) -> Option<CrossingKind> {
    if tags
        .get("waterway")
        .is_some_and(|waterway| osm_waterway(waterway))
    {
        return Some(CrossingKind::Water);
    }
    tags.get("highway")
        .filter(|highway| osm_road_context(highway))
        .map(|_| CrossingKind::Road)
}

const fn osm_context_source(kind: CrossingKind) -> &'static str {
    match kind {
        CrossingKind::Road => "osm-road-context",
        CrossingKind::Water => "osm-hydrology-context",
    }
}

fn osm_waterway(waterway: &str) -> bool {
    matches!(
        waterway,
        "stream" | "river" | "canal" | "drain" | "ditch" | "brook"
    )
}

fn osm_road_context(highway: &str) -> bool {
    matches!(
        highway,
        "motorway"
            | "trunk"
            | "primary"
            | "secondary"
            | "tertiary"
            | "unclassified"
            | "residential"
            | "living_street"
            | "service"
            | "track"
            | "road"
    )
}

fn source_junction_policy(tags: &BTreeMap<String, String>) -> JunctionPolicy {
    let raised_or_buried = ["bridge", "tunnel"].into_iter().any(|key| {
        tags.get(key)
            .is_some_and(|value| !matches!(value.as_str(), "no" | "false" | "0"))
    }) || tags
        .get("layer")
        .and_then(|layer| layer.parse::<i16>().ok())
        .is_some_and(|layer| layer != 0);
    if raised_or_buried {
        JunctionPolicy::GradeSeparatedEndpoints
    } else {
        JunctionPolicy::ExplicitNodes
    }
}

#[derive(Clone, Copy)]
struct Walkway {
    kind: WayKind,
    realm: WayRealm,
    geometry_claim: GeometryClaim,
    crossing_control: CrossingControl,
    standing: TrailStanding,
    marking: TrailMarking,
    terrain: Terrain,
    terrain_confidence: f64,
    access: Access,
    travel: EdgeTravel,
    road_exposure: f64,
    confidence: f64,
}

impl Walkway {
    fn from_tags(tags: &BTreeMap<String, String>, hiking_relation: bool) -> Option<Self> {
        let (highway, standing) = standing_from_tags(tags);
        let route = tags.get("route").map(String::as_str);
        let foot = tags.get("foot").map(String::as_str);
        let hiking = hiking_relation
            || route.is_some_and(|route| matches!(route, "hiking" | "foot" | "walking"));
        let walkable_highway = highway
            .map(WayKind::from_tag)
            .filter(|kind| *kind != WayKind::Unknown);
        if walkable_highway.is_none() && !hiking {
            return None;
        }
        let mut kind = walkable_highway.unwrap_or(WayKind::Path);
        if kind == WayKind::Footway {
            kind = match tags.get("footway").map(String::as_str) {
                Some("sidewalk") => WayKind::Sidewalk,
                Some("crossing" | "traffic_island") => WayKind::Crossing,
                _ => WayKind::Footway,
            };
        }
        let geometry_claim = if matches!(kind, WayKind::ServiceRoad | WayKind::Roadway)
            && asserts_sidewalk(tags)
            && !asserts_separate_sidepath(tags)
        {
            kind = WayKind::Sidewalk;
            GeometryClaim::CenterlineProxy
        } else {
            GeometryClaim::Surveyed
        };
        if matches!(kind, WayKind::Roadway | WayKind::ServiceRoad) && foot == Some("use_sidepath") {
            return None;
        }
        if matches!(kind, WayKind::Roadway | WayKind::ServiceRoad)
            && highway.is_some_and(|highway| matches!(highway, "motorway" | "trunk"))
            && foot.is_none_or(|foot| !matches!(foot, "yes" | "designated" | "permissive"))
        {
            return None;
        }
        let surface = tags.get("surface");
        let surface_terrain =
            surface.map_or(Terrain::Unknown, |surface| Terrain::from_tag(surface));
        let (terrain, terrain_confidence) =
            terrain_from_tags(tags, kind, surface_terrain, surface.is_some());
        let access = access_from_tags(tags, foot);
        let realm = realm_from_way(kind, hiking);
        let road_exposure = match kind {
            WayKind::Roadway | WayKind::ServiceRoad | WayKind::Track => 1.0,
            WayKind::Crossing => 0.65,
            WayKind::Sidewalk if geometry_claim == GeometryClaim::CenterlineProxy => 0.25,
            _ => 0.0,
        };
        Some(Self {
            kind,
            realm,
            geometry_claim,
            crossing_control: crossing_control_from_tags(tags, kind),
            standing,
            marking: marking_from_tags(tags, hiking),
            terrain,
            terrain_confidence,
            access,
            travel: travel_from_tags(tags),
            road_exposure,
            confidence: 0.74,
        })
    }
}

fn marking_from_tags(tags: &BTreeMap<String, String>, hiking_relation: bool) -> TrailMarking {
    tags.get("trailblazed")
        .or_else(|| tags.get("marked"))
        .map_or_else(
            || {
                if hiking_relation || tags.contains_key("trailblazed:visibility") {
                    TrailMarking::Marked
                } else {
                    TrailMarking::Unknown
                }
            },
            |tag| TrailMarking::from_tag(tag),
        )
}

fn standing_from_tags(tags: &BTreeMap<String, String>) -> (Option<&str>, TrailStanding) {
    if let Some(highway) = tags.get("abandoned:highway") {
        return (Some(highway), TrailStanding::Historical);
    }
    if let Some(highway) = tags.get("disused:highway") {
        return (Some(highway), TrailStanding::Unmaintained);
    }
    let highway = tags.get("highway").map(String::as_str);
    let standing = if tags.get("informal").is_some_and(|value| value == "yes") {
        TrailStanding::Informal
    } else if tags
        .get("trail_visibility")
        .is_some_and(|value| matches!(value.as_str(), "bad" | "horrible" | "no"))
        || tags
            .get("maintenance")
            .is_some_and(|value| matches!(value.as_str(), "no" | "none"))
    {
        TrailStanding::Unmaintained
    } else if highway.is_some() {
        TrailStanding::Established
    } else {
        TrailStanding::Unknown
    };
    (highway, standing)
}

fn terrain_from_tags(
    tags: &BTreeMap<String, String>,
    kind: WayKind,
    surface_terrain: Terrain,
    has_surface: bool,
) -> (Terrain, f64) {
    if surface_terrain != Terrain::Unknown {
        return (surface_terrain, 0.86);
    }
    let sac = tags.get("sac_scale").map(String::as_str);
    if sac.is_some_and(|sac| sac.contains("alpine_hiking")) {
        (Terrain::Alpine, 0.80)
    } else if sac.is_some_and(|sac| {
        sac.contains("demanding_mountain_hiking") || sac.contains("demanding_alpine_hiking")
    }) {
        (Terrain::Scramble, 0.80)
    } else {
        match kind {
            WayKind::Sidewalk
            | WayKind::Crossing
            | WayKind::PedestrianStreet
            | WayKind::Cycleway => (Terrain::Pavement, 0.76),
            WayKind::Track | WayKind::ServiceRoad | WayKind::Roadway => (Terrain::Road, 0.62),
            _ if has_surface => (Terrain::Trail, 0.68),
            _ => (Terrain::Trail, 0.50),
        }
    }
}

fn access_from_tags(tags: &BTreeMap<String, String>, foot: Option<&str>) -> Access {
    match foot.or_else(|| tags.get("access").map(String::as_str)) {
        Some("yes" | "designated" | "permissive" | "official") | None => Access::Open,
        Some(tag) => Access::from_tag(tag),
    }
}

fn travel_from_tags(tags: &BTreeMap<String, String>) -> EdgeTravel {
    tags.get("oneway:foot")
        .map_or(EdgeTravel::Both, |tag| EdgeTravel::from_tag(tag))
}

const fn realm_from_way(kind: WayKind, hiking: bool) -> WayRealm {
    match kind {
        WayKind::Path | WayKind::Track | WayKind::Bridleway | WayKind::Bushwhack => {
            WayRealm::Recreational
        }
        WayKind::Footway | WayKind::Steps | WayKind::Cycleway if hiking => WayRealm::Recreational,
        WayKind::Sidewalk
        | WayKind::Crossing
        | WayKind::PedestrianStreet
        | WayKind::ServiceRoad
        | WayKind::Roadway
            if hiking =>
        {
            WayRealm::Connector
        }
        _ => WayRealm::Urban,
    }
}

fn asserts_sidewalk(tags: &BTreeMap<String, String>) -> bool {
    [
        "sidewalk",
        "sidewalk:left",
        "sidewalk:right",
        "sidewalk:both",
    ]
    .into_iter()
    .filter_map(|key| tags.get(key))
    .any(|value| matches!(value.as_str(), "yes" | "both" | "left" | "right"))
}

fn asserts_separate_sidepath(tags: &BTreeMap<String, String>) -> bool {
    tags.get("foot")
        .is_some_and(|value| value == "use_sidepath")
        || [
            "sidewalk",
            "sidewalk:left",
            "sidewalk:right",
            "sidewalk:both",
        ]
        .into_iter()
        .filter_map(|key| tags.get(key))
        .any(|value| value == "separate")
}

fn crossing_control_from_tags(tags: &BTreeMap<String, String>, kind: WayKind) -> CrossingControl {
    if tags
        .get("bridge")
        .or_else(|| tags.get("tunnel"))
        .is_some_and(|value| !matches!(value.as_str(), "no" | "false" | "0"))
    {
        return CrossingControl::GradeSeparated;
    }
    if kind != WayKind::Crossing {
        return CrossingControl::None;
    }
    match tags
        .get("crossing")
        .or_else(|| tags.get("crossing:signals"))
        .map(String::as_str)
    {
        Some("traffic_signals" | "signals" | "yes") => CrossingControl::Signals,
        Some("marked" | "zebra") => CrossingControl::Marked,
        _ => CrossingControl::Uncontrolled,
    }
}

fn xml_tags(node: roxmltree::Node<'_, '_>) -> BTreeMap<String, String> {
    node.children()
        .filter(|child| child.has_tag_name("tag"))
        .filter_map(|tag| {
            Some((
                tag.attribute("k")?.to_ascii_lowercase(),
                tag.attribute("v")?.to_ascii_lowercase(),
            ))
        })
        .collect()
}

fn pbf_tags(tags: &PbfTags) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| {
            (
                key.as_str().to_ascii_lowercase(),
                value.as_str().to_ascii_lowercase(),
            )
        })
        .collect()
}

fn required_attr<'a>(node: roxmltree::Node<'a, '_>, key: &str) -> Result<&'a str> {
    node.attribute(key)
        .ok_or_else(|| TrailgenError::InvalidData(format!("OSM XML missing {key} attribute")))
}

fn way_id(way: roxmltree::Node<'_, '_>) -> String {
    way.attribute("id").unwrap_or("unknown-way").to_owned()
}

fn relation_id(relation: roxmltree::Node<'_, '_>) -> String {
    relation
        .attribute("id")
        .unwrap_or("unknown-relation")
        .to_owned()
}
