//! Bookmark system: built-in frequency catalog + user bookmarks + CubicSDR import.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::audio::demod::DemodMode;

/// A single bookmark entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub freq: f64,
    pub channel_bw: f64,
    pub mode: DemodMode,
    pub category: String,
}

/// Persistent store for user bookmarks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkStore {
    pub user_bookmarks: Vec<Bookmark>,
}

/// Return the built-in frequency catalog (not serialized).
pub fn default_catalog() -> Vec<Bookmark> {
    vec![
        // Aviation (AM) — Madrid Barajas LEMD
        Bookmark { name: "Intl Emergency".into(), freq: 121_500_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid TWR 1".into(), freq: 118_070_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid TWR 2".into(), freq: 118_150_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid TWR 3".into(), freq: 118_670_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid TWR 4".into(), freq: 118_975_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid GND".into(), freq: 121_850_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid APP".into(), freq: 127_500_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid ATIS 1".into(), freq: 118_250_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        Bookmark { name: "Madrid ATIS 2".into(), freq: 130_850_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "✈️ Aviation".into() },
        // FM Radio
        Bookmark { name: "Cadena SER Madrid".into(), freq: 105_400_000.0, channel_bw: 200_000.0, mode: DemodMode::FM, category: "📻 FM Radio".into() },
        Bookmark { name: "RNE Radio Nacional".into(), freq: 100_000_000.0, channel_bw: 200_000.0, mode: DemodMode::FM, category: "📻 FM Radio".into() },
        Bookmark { name: "Los 40 Principales".into(), freq: 93_900_000.0, channel_bw: 200_000.0, mode: DemodMode::FM, category: "📻 FM Radio".into() },
        // PMR446 (NFM)
        Bookmark { name: "PMR446 Ch 1".into(), freq: 446_006_250.0, channel_bw: 12_500.0, mode: DemodMode::NFM, category: "📡 PMR446".into() },
        Bookmark { name: "PMR446 Ch 2".into(), freq: 446_018_750.0, channel_bw: 12_500.0, mode: DemodMode::NFM, category: "📡 PMR446".into() },
        Bookmark { name: "PMR446 Ch 3".into(), freq: 446_031_250.0, channel_bw: 12_500.0, mode: DemodMode::NFM, category: "📡 PMR446".into() },
        // Marine VHF (NFM)
        Bookmark { name: "Marine Ch 16 Emergency".into(), freq: 156_800_000.0, channel_bw: 25_000.0, mode: DemodMode::NFM, category: "⚓ Marine VHF".into() },
    ]
}

fn bookmarks_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/termsdr/bookmarks.json")
}

impl BookmarkStore {
    /// Load user bookmarks from disk, or return empty store.
    pub fn load() -> Self {
        let path = bookmarks_path();
        if let Ok(data) = fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or(Self { user_bookmarks: Vec::new() })
        } else {
            Self { user_bookmarks: Vec::new() }
        }
    }

    /// Save user bookmarks to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = bookmarks_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Add a bookmark and persist.
    pub fn add(&mut self, bookmark: Bookmark) {
        self.user_bookmarks.push(bookmark);
        self.save().ok();
    }

    /// Remove a bookmark by index and persist.
    pub fn remove(&mut self, index: usize) {
        if index < self.user_bookmarks.len() {
            self.user_bookmarks.remove(index);
            self.save().ok();
        }
    }

    /// Remove a bookmark by name (first match). Returns true if found.
    pub fn remove_by_name(&mut self, name: &str) -> bool {
        if let Some(idx) = self.user_bookmarks.iter().position(|b| b.name == name) {
            self.user_bookmarks.remove(idx);
            self.save().ok();
            true
        } else {
            false
        }
    }
}

/// Import bookmarks from a CubicSDR XML file using simple string parsing.
pub fn import_cubicsdr(path: &str) -> anyhow::Result<Vec<Bookmark>> {
    let content = fs::read_to_string(path)?;
    let mut bookmarks = Vec::new();

    // Split by <demodulator> blocks
    let mut rest = content.as_str();
    while let Some(start) = rest.find("<demodulator>") {
        let after_start = &rest[start + 13..];
        let end = after_start.find("</demodulator>").unwrap_or(after_start.len());
        let block = &after_start[..end];
        rest = &after_start[end..];

        let frequency = extract_xml_tag(block, "frequency")
            .and_then(|s| s.parse::<f64>().ok());
        let bandwidth = extract_xml_tag(block, "bandwidth")
            .and_then(|s| s.parse::<f64>().ok());
        let demod_type = extract_xml_tag(block, "type");
        let user_label = extract_xml_tag(block, "user_label").unwrap_or_default();

        if let (Some(freq), Some(bw)) = (frequency, bandwidth) {
            let mode = match demod_type.as_deref() {
                Some("AM") => DemodMode::AM,
                Some("FM") | Some("FMS") => DemodMode::FM,
                Some("NFM") => DemodMode::NFM,
                _ => DemodMode::AM,
            };
            let name = if user_label.is_empty() {
                format!("{:.3} MHz", freq / 1_000_000.0)
            } else {
                user_label
            };
            bookmarks.push(Bookmark {
                name,
                freq,
                channel_bw: bw,
                mode,
                category: "📥 Imported".into(),
            });
        }
    }

    Ok(bookmarks)
}

/// Extract text content from a simple XML tag like `<tag>value</tag>`.
fn extract_xml_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = block.find(&open)?;
    let after = start + open.len();
    let end = block[after..].find(&close)?;
    let value = block[after..after + end].trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_default_catalog_count() {
        let catalog = default_catalog();
        assert_eq!(catalog.len(), 16);
    }

    #[test]
    fn test_default_catalog_frequencies() {
        let catalog = default_catalog();
        let emergency = catalog.iter().find(|b| b.name == "Intl Emergency").unwrap();
        assert!((emergency.freq - 121_500_000.0).abs() < 1.0);
        assert_eq!(emergency.mode, DemodMode::AM);
    }

    #[test]
    fn test_default_catalog_categories() {
        let catalog = default_catalog();
        let categories: Vec<&str> = catalog.iter().map(|b| b.category.as_str()).collect();
        assert!(categories.contains(&"✈️ Aviation"));
        assert!(categories.contains(&"📻 FM Radio"));
        assert!(categories.contains(&"📡 PMR446"));
        assert!(categories.contains(&"⚓ Marine VHF"));
    }

    #[test]
    fn test_bookmark_store_load_empty() {
        // With no file, should return empty
        let store = BookmarkStore { user_bookmarks: Vec::new() };
        assert!(store.user_bookmarks.is_empty());
    }

    #[test]
    fn test_bookmark_store_add_remove() {
        let mut store = BookmarkStore { user_bookmarks: Vec::new() };
        store.user_bookmarks.push(Bookmark {
            name: "Test".into(),
            freq: 100_000_000.0,
            channel_bw: 25_000.0,
            mode: DemodMode::AM,
            category: "Test".into(),
        });
        assert_eq!(store.user_bookmarks.len(), 1);
        store.user_bookmarks.remove(0);
        assert!(store.user_bookmarks.is_empty());
    }

    #[test]
    fn test_bookmark_store_remove_by_name() {
        let mut store = BookmarkStore {
            user_bookmarks: vec![
                Bookmark { name: "A".into(), freq: 100e6, channel_bw: 25e3, mode: DemodMode::AM, category: "T".into() },
                Bookmark { name: "B".into(), freq: 200e6, channel_bw: 25e3, mode: DemodMode::FM, category: "T".into() },
            ],
        };
        assert!(store.remove_by_name("A"));
        assert_eq!(store.user_bookmarks.len(), 1);
        assert_eq!(store.user_bookmarks[0].name, "B");
        assert!(!store.remove_by_name("C"));
    }

    #[test]
    fn test_bookmark_serialization_roundtrip() {
        let store = BookmarkStore {
            user_bookmarks: vec![
                Bookmark { name: "Test".into(), freq: 118_070_000.0, channel_bw: 6_000.0, mode: DemodMode::AM, category: "⭐ My Bookmarks".into() },
            ],
        };
        let json = serde_json::to_string(&store).unwrap();
        let loaded: BookmarkStore = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.user_bookmarks.len(), 1);
        assert_eq!(loaded.user_bookmarks[0].name, "Test");
        assert!((loaded.user_bookmarks[0].freq - 118_070_000.0).abs() < 1.0);
        assert_eq!(loaded.user_bookmarks[0].mode, DemodMode::AM);
    }

    #[test]
    fn test_demod_mode_serialization() {
        let json = serde_json::to_string(&DemodMode::AM).unwrap();
        let loaded: DemodMode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, DemodMode::AM);

        let json = serde_json::to_string(&DemodMode::FM).unwrap();
        let loaded: DemodMode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, DemodMode::FM);

        let json = serde_json::to_string(&DemodMode::NFM).unwrap();
        let loaded: DemodMode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, DemodMode::NFM);
    }

    #[test]
    fn test_import_cubicsdr() {
        let xml = r#"<?xml version="1.0" ?>
<cubicsdr_session>
 <header>
  <version>%30%2e%32%2e%37</version>
  <center_freq>118148160</center_freq>
 </header>
 <demodulators>
  <demodulator>
   <bandwidth>6000</bandwidth>
   <frequency>118148160</frequency>
   <type>AM</type>
   <user_label></user_label>
  </demodulator>
  <demodulator>
   <bandwidth>6000</bandwidth>
   <frequency>118975200</frequency>
   <type>AM</type>
   <user_label>Tower 2</user_label>
  </demodulator>
 </demodulators>
</cubicsdr_session>"#;

        let dir = std::env::temp_dir().join("termsdr_test_import");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.xml");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(xml.as_bytes()).unwrap();

        let bookmarks = import_cubicsdr(path.to_str().unwrap()).unwrap();
        assert_eq!(bookmarks.len(), 2);

        // First has no label → auto-generated name
        assert!((bookmarks[0].freq - 118_148_160.0).abs() < 1.0);
        assert!((bookmarks[0].channel_bw - 6_000.0).abs() < 1.0);
        assert_eq!(bookmarks[0].mode, DemodMode::AM);
        assert_eq!(bookmarks[0].category, "📥 Imported");
        assert!(bookmarks[0].name.contains("118.148"));

        // Second has label
        assert_eq!(bookmarks[1].name, "Tower 2");
        assert!((bookmarks[1].freq - 118_975_200.0).abs() < 1.0);

        // Cleanup
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_import_cubicsdr_file_not_found() {
        let result = import_cubicsdr("/nonexistent/path.xml");
        assert!(result.is_err());
    }

    #[test]
    fn test_import_cubicsdr_empty_xml() {
        let dir = std::env::temp_dir().join("termsdr_test_empty");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.xml");
        fs::write(&path, "<cubicsdr_session></cubicsdr_session>").unwrap();

        let bookmarks = import_cubicsdr(path.to_str().unwrap()).unwrap();
        assert!(bookmarks.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_extract_xml_tag() {
        assert_eq!(extract_xml_tag("<frequency>123</frequency>", "frequency"), Some("123".into()));
        assert_eq!(extract_xml_tag("<type>AM</type>", "type"), Some("AM".into()));
        assert_eq!(extract_xml_tag("<user_label></user_label>", "user_label"), None);
        assert_eq!(extract_xml_tag("no tags here", "frequency"), None);
    }

    #[test]
    fn test_bookmark_store_save_load_roundtrip() {
        // Use a temp path
        let dir = std::env::temp_dir().join("termsdr_test_store");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bookmarks.json");

        let store = BookmarkStore {
            user_bookmarks: vec![
                Bookmark { name: "Saved".into(), freq: 100e6, channel_bw: 25e3, mode: DemodMode::NFM, category: "⭐ My Bookmarks".into() },
            ],
        };
        let json = serde_json::to_string_pretty(&store).unwrap();
        fs::write(&path, &json).unwrap();

        let loaded: BookmarkStore = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.user_bookmarks.len(), 1);
        assert_eq!(loaded.user_bookmarks[0].name, "Saved");
        assert_eq!(loaded.user_bookmarks[0].mode, DemodMode::NFM);

        fs::remove_dir_all(&dir).ok();
    }
}
