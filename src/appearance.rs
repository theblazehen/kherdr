use std::{fs, io::Read, path::Path};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSize {
    #[default]
    Desktop,
    Comfort,
    Large,
}

impl TextSize {
    pub fn from_index(index: i32) -> Option<Self> {
        match index { 0 => Some(Self::Desktop), 1 => Some(Self::Comfort), 2 => Some(Self::Large), _ => None }
    }
    pub fn index(self) -> i32 {
        match self { Self::Desktop => 0, Self::Comfort => 1, Self::Large => 2 }
    }
    // Physical pixels at scale 1. Desktop matches a measured 6×12 px Ghostty
    // cell on the user's ~118 PPI Dell, projected onto the 300 PPI Kindle.
    pub fn metrics(self) -> (u16, u16, f32) {
        match self { Self::Desktop => (15, 31, 25.0), Self::Comfort => (22, 46, 36.0), Self::Large => (29, 62, 48.0) }
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("Cannot read text size {}: {error}", path.display())),
        };
        let mut bytes = Vec::new();
        file.take(1025).read_to_end(&mut bytes).map_err(|error| format!("Cannot read text size: {error}"))?;
        if bytes.len() > 1024 { return Err("Text size preference exceeds 1024 bytes".into()); }
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid text size {}: {error}", path.display()))
    }
    pub fn save(self, path: &Path) -> Result<(), String> {
        crate::atomic_file::replace(path, &serde_json::to_vec(&self).map_err(|error| error.to_string())?,
            1024, crate::atomic_file::DirectorySync::BestEffort)
    }
}
