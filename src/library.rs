
use std::path::PathBuf;

/// Returns the shared Patchwork custom-node library directory.
pub fn library_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA")
        .ok_or("APPDATA environment variable not found")?;

    let path = PathBuf::from(appdata)
        .join("Patchwork")
        .join("library")
        .join("nodes");

    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Failed to create library directory: {e}"))?;

    Ok(path)
}


/// Returns all .pwnode files directly inside the shared library directory.
pub fn list_library_nodes() -> Result<Vec<PathBuf>, String> {
    let directory = library_dir()?;
    let mut nodes = Vec::new();

    let entries = std::fs::read_dir(&directory)
        .map_err(|e| format!("Failed to read library directory: {e}"))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| format!("Failed to read library entry: {e}"))?;

        let path = entry.path();

        if path.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("pwnode")
        {
            nodes.push(path);
        }
    }

    nodes.sort();

    Ok(nodes)
}
/// Publish a standalone custom node to the shared Library.
/// Refuses to overwrite an existing library node.
pub fn publish_to_library(
    source: &std::path::Path,
) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err("Custom node file does not exist.".to_string());
    }

    if source.extension().and_then(|e| e.to_str()) != Some("pwnode") {
        return Err("Only .pwnode files can be published.".to_string());
    }

    let file_name = source
        .file_name()
        .ok_or("Invalid custom node filename.")?;

    let destination = library_dir()?.join(file_name);

    if destination.exists() {
        return Err(format!(
            "A Library node named '{}' already exists.",
            file_name.to_string_lossy()
        ));
    }

    // Exclusive creation prevents overwriting an existing node.
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|e| format!("Failed to create Library node: {e}"))?;

    let result = (|| -> Result<(), String> {
        let mut input = std::fs::File::open(source)
            .map_err(|e| format!("Failed to read custom node: {e}"))?;

        std::io::copy(&mut input, &mut output)
            .map_err(|e| format!("Failed to copy custom node: {e}"))?;

        Ok(())
    })();

    if let Err(error) = result {
        drop(output);
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }

    Ok(destination)
}
