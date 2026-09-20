use std::{path::PathBuf, sync::Arc};
use slint::fontique_010::{fontique, shared_collection};

/// Register the device's real font files rather than depending on its reduced
/// Fontconfig ABI. No font files are redistributed with the application.
pub fn register() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::var_os("KHERDR_FONT_DIR").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/java/lib/fonts"));
    let mut collection = shared_collection();
    let mut sans = Vec::new();
    let mut mono = Vec::new();
    let mut fallback = Vec::new();
    for (filename, category) in [
        ("Amazon-Ember-Regular.ttf",0), ("Amazon-Ember-Bold.ttf",0),
        ("KindleBlackboxRegular.ttf",1), ("KindleBlackboxBold.ttf",1),
        ("KindleBlackboxItalic.ttf",1), ("KindleBlackboxBoldItalic.ttf",1),
        ("TBGothicMed_213.ttf",2), ("code2000.ttf",2),
    ] {
        let path = directory.join(filename);
        let bytes = std::fs::read(&path).map_err(|error| format!("Cannot read font {}: {error}",path.display()))?;
        let families = collection.register_fonts(fontique::Blob::new(Arc::new(bytes)),None);
        if families.is_empty() { return Err(format!("No fonts found in {}",path.display()).into()); }
        let ids = match category { 0 => &mut sans, 1 => &mut mono, _ => &mut fallback };
        for (id,_) in families { if !ids.contains(&id) { ids.push(id); } }
    }
    collection.set_generic_families(fontique::GenericFamily::SansSerif,sans.iter().chain(&fallback).copied());
    collection.set_generic_families(fontique::GenericFamily::Serif,sans.iter().chain(&fallback).copied());
    collection.set_generic_families(fontique::GenericFamily::Monospace,mono.iter().chain(&fallback).copied());
    for script in ["Latn","Grek","Cyrl","Hani","Hira","Kana","Zyyy","Zinh","Arab","Hebr"] {
        collection.set_fallbacks(fontique::FallbackKey::new(fontique::Script::from_str_unchecked(script),None),
            fallback.iter().chain(&sans).copied());
    }
    eprintln!("kherdr: explicitly registered Kindle fonts from {}",directory.display());
    Ok(())
}
