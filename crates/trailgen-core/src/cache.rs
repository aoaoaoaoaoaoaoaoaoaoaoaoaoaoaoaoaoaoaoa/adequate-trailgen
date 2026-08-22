use crate::{Edge, Result, TrailgenError, TurnBan, Vertex, WalkGraph};
use serde::{Deserialize, Serialize as _};

pub const GRAPH_CACHE: &str = "cache/graph.bin";
const MAGIC: &[u8; 16] = b"TRAILGEN-GRAPH\0\0";
const FORMAT: u16 = 2;

pub fn encode_graph(graph: &WalkGraph) -> Result<Vec<u8>> {
    graph.validate()?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT.to_le_bytes());
    let mut encoder = zstd::stream::write::Encoder::new(bytes, 3)
        .map_err(|error| TrailgenError::InvalidData(format!("open graph encoder: {error}")))?;
    graph
        .serialize(&mut rmp_serde::Serializer::new(&mut encoder).with_struct_map())
        .map_err(|error| TrailgenError::InvalidData(format!("encode graph cache: {error}")))?;
    let bytes = encoder
        .finish()
        .map_err(|error| TrailgenError::InvalidData(format!("finish graph cache: {error}")))?;
    Ok(bytes)
}

pub fn decode_graph(bytes: &[u8]) -> Result<WalkGraph> {
    let Some((header, body)) = bytes.split_at_checked(MAGIC.len() + size_of::<u16>()) else {
        return Err(TrailgenError::InvalidData(
            "graph cache header is truncated".to_owned(),
        ));
    };
    if &header[..MAGIC.len()] != MAGIC {
        return Err(TrailgenError::InvalidData(
            "graph cache has the wrong format signature".to_owned(),
        ));
    }
    let format = u16::from_le_bytes(
        header[MAGIC.len()..]
            .try_into()
            .expect("header length was checked"),
    );
    if format != FORMAT {
        return Err(TrailgenError::InvalidData(format!(
            "graph cache format {format} is unsupported"
        )));
    }
    let decoder = zstd::stream::read::Decoder::new(body)
        .map_err(|error| TrailgenError::InvalidData(format!("open graph cache: {error}")))?;
    let stored = rmp_serde::from_read::<_, CachedGraph>(decoder)
        .map_err(|error| TrailgenError::InvalidData(format!("decode graph cache: {error}")))?;
    let mut graph = WalkGraph {
        vertices: stored.vertices,
        edges: stored.edges,
        turn_bans: stored.turn_bans,
        adjacency: Vec::new(),
    };
    graph.validate()?;
    graph.rebuild_adjacency();
    Ok(graph)
}

#[derive(Deserialize)]
struct CachedGraph {
    vertices: Vec<Vertex>,
    edges: Vec<Edge>,
    #[serde(default)]
    turn_bans: Vec<TurnBan>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphBuilder, io::geojson};

    #[test]
    fn graph_cache_is_compact_exact_and_self_identifying() {
        let graph = GraphBuilder::default()
            .build(
                &geojson::network_from_str(include_str!("../tests/fixtures/mini_network.geojson"))
                    .unwrap(),
            )
            .unwrap();
        let encoded = encode_graph(&graph).unwrap();

        assert_eq!(decode_graph(&encoded).unwrap(), graph);
        assert!(encoded.len() < serde_json::to_vec(&graph).unwrap().len());
        let mut corrupt = encoded;
        corrupt[0] ^= 0xff;
        assert!(decode_graph(&corrupt).is_err());
    }
}
