//! Crash-safe JSON persistence (temp file + fsync + rename), same layout as the JS bots:
//! pretty-printed with two-space indentation and a trailing newline.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads and deserializes a JSON file. Returns `Ok(None)` when the file does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("{} contains invalid JSON", path.display()))?;
    Ok(Some(value))
}

/// Serializes `value` and atomically replaces `path` with it.
pub fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let mut json = serde_json::to_vec_pretty(value).context("serialize JSON")?;
    json.push(b'\n');
    write_atomic(path, &json)
}

/// Atomically replaces `path` with `bytes`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;

    let tmp = temp_path(path);
    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("write {}", path.display()))
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_owned());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let dir = std::env::temp_dir().join(format!("operator-common-fs-{}", std::process::id()));
        let path = dir.join("nested/state.json");
        assert!(read_json::<serde_json::Value>(&path).unwrap().is_none());

        write_json_atomic(&path, &serde_json::json!({ "a": 1 })).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "{\n  \"a\": 1\n}\n");
        let value: serde_json::Value = read_json(&path).unwrap().unwrap();
        assert_eq!(value["a"], 1);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
