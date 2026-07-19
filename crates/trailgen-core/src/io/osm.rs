use crate::builder::{JunctionPolicy, SegmentDraft, TurnRestrictionDraft, TurnRestrictionRule};
use crate::geo::{Coord, LineString};
use crate::model::{Access, CrossingKind, EdgeTravel, Provenance, Terrain};
use crate::overlay::ContextOverlay;
use crate::{Result, TrailgenError};
use osmpbfreader::{
    Node as PbfNode, OsmId, OsmObj, OsmPbfReader, Relation as PbfRelation, Tags as PbfTags,
    Way as PbfWay, WayId,
};
use std::collections::BTreeMap;
use std::io::{Read, Seek};

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
    root.children()
        .filter(|node| node.has_tag_name("way"))
        .filter_map(|way| draft_from_xml_way(way, &nodes, &relations).transpose())
        .collect()
}

pub fn network_from_pbf_reader<R: Read + Seek>(reader: R) -> Result<Vec<SegmentDraft>> {
    let mut pbf = OsmPbfReader::new(reader);
    let objects = pbf
        .get_objs_and_deps(pbf_network_object)
        .map_err(|error| TrailgenError::InvalidData(format!("parse OSM PBF: {error}")))?;
    let relations = pbf_relation_evidence(&objects);
    objects
        .values()
        .filter_map(|object| match object {
            OsmObj::Way(way) => draft_from_pbf_way(way, &objects, &relations).transpose(),
            OsmObj::Node(_) | OsmObj::Relation(_) => None,
        })
        .collect()
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

fn parse_node(node: roxmltree::Node<'_, '_>) -> Result<(String, Coord)> {
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
    let ele = xml_tags(node)
        .get("ele")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite());
    Ok((id, Coord { lon, lat, ele }))
}

fn draft_from_xml_way(
    way: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, Coord>,
    relations: &OsmRelationEvidence,
) -> Result<Option<SegmentDraft>> {
    let tags = xml_tags(way);
    let Some(walkway) = Walkway::from_tags(&tags) else {
        return Ok(None);
    };
    let id = way_id(way);
    let mut points = Vec::new();
    for nd in way.children().filter(|node| node.has_tag_name("nd")) {
        let reference = required_attr(nd, "ref")?;
        let Some(coord) = nodes.get(reference) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM way {id} references missing node {reference}"
            )));
        };
        points.push(*coord);
    }
    if points.len() < 2 {
        return Ok(None);
    }
    Ok(Some(SegmentDraft {
        junctions: JunctionPolicy::ExplicitNodes,
        turn_ref: Some(id.clone()),
        turn_restrictions: relations.turn_restrictions.clone(),
        geometry: LineString::new(points)?,
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
        provenance: osm_way_provenance("osm-xml", &id, relations),
    }))
}

fn context_from_xml_way(
    way: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, Coord>,
) -> Result<Option<ContextOverlay>> {
    let tags = xml_tags(way);
    let Some(kind) = context_kind_from_tags(&tags) else {
        return Ok(None);
    };
    let id = way_id(way);
    let mut points = Vec::new();
    for nd in way.children().filter(|node| node.has_tag_name("nd")) {
        let reference = required_attr(nd, "ref")?;
        let Some(coord) = nodes.get(reference) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM way {id} references missing node {reference}"
            )));
        };
        points.push(*coord);
    }
    context_overlay(kind, &tags, id, points)
}

fn pbf_walkable_way(object: &OsmObj) -> bool {
    object
        .way()
        .is_some_and(|way| Walkway::from_tags(&pbf_tags(&way.tags)).is_some())
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
) -> Result<Option<SegmentDraft>> {
    let tags = pbf_tags(&way.tags);
    let Some(walkway) = Walkway::from_tags(&tags) else {
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
    if points.len() < 2 {
        return Ok(None);
    }
    Ok(Some(SegmentDraft {
        junctions: JunctionPolicy::ExplicitNodes,
        turn_ref: Some(way.id.0.to_string()),
        turn_restrictions: relations.turn_restrictions.clone(),
        geometry: LineString::new(points)?,
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
        provenance: osm_way_provenance("osm-pbf", &way.id.0.to_string(), relations),
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

fn xml_relation_evidence(
    root: roxmltree::Node<'_, '_>,
    nodes: &BTreeMap<String, Coord>,
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
        for member in relation
            .children()
            .filter(|node| node.has_tag_name("member"))
        {
            let member_type = required_attr(member, "type")?;
            let role = required_attr(member, "role")?;
            if member_type == "node" && role == "via" {
                if let Some(coord) = nodes.get(required_attr(member, "ref")?) {
                    via = Some(*coord);
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
        for reference in &relation.refs {
            if let OsmId::Node(node_id) = reference.member
                && reference.role.as_str() == "via"
                && let Some(OsmObj::Node(node)) = objects.get(&OsmId::Node(node_id))
            {
                via = Some(pbf_coord(node));
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
    tags.get("restriction")
        .or_else(|| tags.get("restriction:foot"))
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

#[derive(Clone, Copy)]
struct Walkway {
    terrain: Terrain,
    terrain_confidence: f64,
    access: Access,
    travel: EdgeTravel,
    road_exposure: f64,
    confidence: f64,
}

impl Walkway {
    fn from_tags(tags: &BTreeMap<String, String>) -> Option<Self> {
        let highway = tags.get("highway").map(String::as_str);
        let route = tags.get("route").map(String::as_str);
        let foot = tags.get("foot").map(String::as_str);
        let hiking = route.is_some_and(|route| matches!(route, "hiking" | "foot"));
        let walkable_highway = highway.and_then(WalkwayKind::from_highway);
        if walkable_highway.is_none() && !hiking {
            return None;
        }
        let kind = walkable_highway.unwrap_or(WalkwayKind::Path);
        let surface = tags.get("surface");
        let surface_terrain =
            surface.map_or(Terrain::Unknown, |surface| Terrain::from_tag(surface));
        let (terrain, terrain_confidence) =
            terrain_from_tags(tags, kind, surface_terrain, surface.is_some());
        let access = access_from_tags(tags, foot);
        let road_exposure =
            f64::from(kind.road_like() || matches!(terrain, Terrain::Road | Terrain::Pavement));
        Some(Self {
            terrain,
            terrain_confidence,
            access,
            travel: travel_from_tags(tags),
            road_exposure,
            confidence: 0.74,
        })
    }
}

#[derive(Clone, Copy)]
enum WalkwayKind {
    Path,
    Footway,
    Track,
    Service,
    Pedestrian,
    Steps,
    Bridleway,
    Road,
}

impl WalkwayKind {
    fn from_highway(highway: &str) -> Option<Self> {
        Some(match highway {
            "path" => Self::Path,
            "footway" => Self::Footway,
            "track" => Self::Track,
            "service" => Self::Service,
            "pedestrian" => Self::Pedestrian,
            "steps" => Self::Steps,
            "bridleway" => Self::Bridleway,
            "unclassified" | "residential" | "tertiary" | "road" => Self::Road,
            _ => return None,
        })
    }

    const fn road_like(self) -> bool {
        matches!(self, Self::Track | Self::Service | Self::Road)
    }
}

fn terrain_from_tags(
    tags: &BTreeMap<String, String>,
    kind: WalkwayKind,
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
            WalkwayKind::Track | WalkwayKind::Service | WalkwayKind::Road => (Terrain::Road, 0.62),
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
        .or_else(|| tags.get("oneway"))
        .map_or(EdgeTravel::Both, |tag| EdgeTravel::from_tag(tag))
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
