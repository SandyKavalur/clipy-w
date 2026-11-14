//! Persistent user-managed snippet folders.

use serde::{Deserialize, Serialize};
use std::io;

use crate::config::snippets_path;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SnippetStore {
    pub folders: Vec<SnippetFolder>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnippetFolder {
    pub name: String,
    pub snippets: Vec<Snippet>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snippet {
    pub title: String,
    pub text: String,
}

impl SnippetStore {
    pub fn load() -> io::Result<Self> {
        match std::fs::read(snippets_path()) {
            Ok(data) => serde_json::from_slice(&data)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = snippets_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        std::fs::write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_store_roundtrips_json() {
        let store = SnippetStore {
            folders: vec![SnippetFolder {
                name: "Writing".to_owned(),
                snippets: vec![Snippet {
                    title: "Greeting".to_owned(),
                    text: "Hello, world!".to_owned(),
                }],
            }],
        };
        let encoded = serde_json::to_vec(&store).unwrap();
        let decoded: SnippetStore = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.folders[0].name, "Writing");
        assert_eq!(decoded.folders[0].snippets[0].text, "Hello, world!");
    }
}