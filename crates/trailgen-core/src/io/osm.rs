use crate::builder::SegmentDraft;
use crate::geo::{Coord, LineString};
use crate::model::{Access, CrossingKind, EdgeTravel, Provenance, Terrain};
use crate::overlay::ContextOverlay;
use crate::{Result, TrailgenError};
use osmpbfreader::{Node as PbfNode, OsmId, OsmObj, OsmPbfReader, Tags as PbfTags, Way as PbfWay};
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
    root.children()
        .filter(|node| node.has_tag_name("way"))
        .filter_map(|way| draft_from_xml_way(way, &nodes).transpose())
        .collect()
}

pub fn network_from_pbf_reader<R: Read + Seek>(reader: R) -> Result<Vec<SegmentDraft>> {
    let mut pbf = OsmPbfReader::new(reader);
    let objects = pbf
        .get_objs_and_deps(pbf_walkable_way)
        .map_err(|error| TrailgenError::InvalidData(format!("parse OSM PBF: {error}")))?;
    objects
        .values()
        .filter_map(|object| match object {
            OsmObj::Way(way) => draft_from_pbf_way(way, &objects).transpose(),
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
) -> Result<Option<SegmentDraft>> {
    let tags = xml_tags(way);
    let Some(walkway) = Walkway::from_tags(&tags) else {
        return Ok(None);
    };
    let mut points = Vec::new();
    for nd in way.children().filter(|node| node.has_tag_name("nd")) {
        let reference = required_attr(nd, "ref")?;
        let Some(coord) = nodes.get(reference) else {
            return Err(TrailgenError::InvalidData(format!(
                "OSM way {} references missing node {reference}",
                way_id(way)
            )));
        };
        points.push(*coord);
    }
    if points.len() < 2 {
        return Ok(None);
    }
    Ok(Some(SegmentDraft {
        geometry: LineString::new(points)?,
        terrain: walkway.terrain,
        surface: tags.get("surface").cloned(),
        access: walkway.access,
        travel: walkway.travel,
        road_exposure: walkway.road_exposure,
        confidence: walkway.confidence,
        provenance: Provenance {
            source: "osm-xml".to_owned(),
            layer: Some("way".to_owned()),
            source_id: Some(way_id(way)),
            license: Some("ODbL-1.0".to_owned()),
        },
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

fn pbf_context_way(object: &OsmObj) -> bool {
    object
        .way()
        .is_some_and(|way| context_kind_from_tags(&pbf_tags(&way.tags)).is_some())
}

fn draft_from_pbf_way(
    way: &PbfWay,
    objects: &BTreeMap<OsmId, OsmObj>,
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
        geometry: LineString::new(points)?,
        terrain: walkway.terrain,
        surface: tags.get("surface").cloned(),
        access: walkway.access,
        travel: walkway.travel,
        road_exposure: walkway.road_exposure,
        confidence: walkway.confidence,
        provenance: Provenance {
            source: "osm-pbf".to_owned(),
            layer: Some("way".to_owned()),
            source_id: Some(way.id.0.to_string()),
            license: Some("ODbL-1.0".to_owned()),
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
        let surface_terrain = tags
            .get("surface")
            .map_or(Terrain::Unknown, |surface| Terrain::from_tag(surface));
        let terrain = terrain_from_tags(tags, kind, surface_terrain);
        let access = access_from_tags(tags, foot);
        let road_exposure =
            f64::from(kind.road_like() || matches!(terrain, Terrain::Road | Terrain::Pavement));
        Some(Self {
            terrain,
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
) -> Terrain {
    if surface_terrain != Terrain::Unknown {
        return surface_terrain;
    }
    let sac = tags.get("sac_scale").map(String::as_str);
    if sac.is_some_and(|sac| sac.contains("alpine_hiking")) {
        Terrain::Alpine
    } else if sac.is_some_and(|sac| {
        sac.contains("demanding_mountain_hiking") || sac.contains("demanding_alpine_hiking")
    }) {
        Terrain::Scramble
    } else {
        match kind {
            WalkwayKind::Track | WalkwayKind::Service | WalkwayKind::Road => Terrain::Road,
            _ => Terrain::Trail,
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
