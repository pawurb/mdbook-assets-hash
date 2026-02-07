use md5::{Digest, Md5};
use mdbook_preprocessor::{
    Preprocessor, PreprocessorContext,
    book::{Book, BookItem, Chapter},
    errors::Result,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

static DIRECTIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{#asset-hash\s+([^}]+?)\s*\}\}").unwrap());

const DEFAULT_HASH_LENGTH: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub hash: String,
    pub hashed_path: String,
}

pub type Manifest = BTreeMap<String, ManifestEntry>;

#[derive(Default)]
pub struct AssetsHash;

impl AssetsHash {
    pub fn new() -> AssetsHash {
        AssetsHash
    }
}

impl Preprocessor for AssetsHash {
    fn name(&self) -> &str {
        "assets-hash"
    }

    fn run(&self, ctx: &PreprocessorContext, book: Book) -> Result<Book> {
        let hash_length: usize = ctx
            .config
            .get::<usize>("preprocessor.assets-hash.hash-length")
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_HASH_LENGTH);

        let src_dir = ctx.root.join(&ctx.config.book.src);

        // Cleanup old hashed copies (idempotency)
        let manifest_path = ctx.root.join("assets-manifest.json");
        if let Ok(contents) = fs::read_to_string(&manifest_path)
            && let Ok(old_manifest) = serde_json::from_str::<Manifest>(&contents)
        {
            for entry in old_manifest.values() {
                let hashed_file = src_dir.join(&entry.hashed_path);
                if hashed_file.exists()
                    && let Err(e) = fs::remove_file(&hashed_file)
                {
                    eprintln!(
                        "Warning: failed to remove old hashed file {}: {e}",
                        hashed_file.display()
                    );
                }
            }
        }

        let mut manifest = Manifest::new();
        let mut book = book;

        book.for_each_mut(|item: &mut BookItem| {
            if let BookItem::Chapter(ref mut chapter) = *item {
                process_chapter(chapter, &src_dir, hash_length, &mut manifest);
            }
        });

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(&manifest_path, manifest_json)?;

        Ok(book)
    }

    fn supports_renderer(&self, renderer: &str) -> Result<bool> {
        Ok(renderer == "html")
    }
}

fn process_chapter(
    chapter: &mut Chapter,
    src_dir: &Path,
    hash_length: usize,
    manifest: &mut Manifest,
) {
    let Some(chapter_path) = &chapter.path else {
        return;
    };
    let chapter_dir = chapter_path.parent().unwrap_or(Path::new(""));

    let content = chapter.content.clone();
    let new_content = DIRECTIVE_RE
        .replace_all(&content, |caps: &regex::Captures| {
            let asset_path_str = caps[1].trim();
            let asset_path = Path::new(asset_path_str);

            let normalized_key = normalize_path(&chapter_dir.join(asset_path));
            let manifest_key = normalized_key.to_string_lossy().into_owned();

            if let Some(entry) = manifest.get(&manifest_key) {
                let key_path = Path::new(&manifest_key);
                let hashed_name = hashed_filename(
                    key_path.file_stem().unwrap().to_str().unwrap(),
                    &entry.hash,
                    key_path
                        .extension()
                        .map(|e| e.to_str().unwrap())
                        .unwrap_or(""),
                );
                return rewrite_asset_path(asset_path_str, &hashed_name);
            }

            let abs_path = src_dir.join(chapter_dir).join(asset_path);
            let abs_path = match abs_path.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "Warning: cannot resolve asset '{}': {e}",
                        abs_path.display()
                    );
                    return caps[0].to_string();
                }
            };

            let canonical_src = match src_dir.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Warning: cannot canonicalize src dir: {e}");
                    return caps[0].to_string();
                }
            };
            let rel_path = match abs_path.strip_prefix(&canonical_src) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!(
                        "Warning: asset '{}' is outside src dir, skipping",
                        abs_path.display()
                    );
                    return caps[0].to_string();
                }
            };
            let manifest_key = rel_path.to_string_lossy().into_owned();

            let full_hash = match compute_hash(&abs_path) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Warning: failed to hash '{}': {e}", abs_path.display());
                    return caps[0].to_string();
                }
            };
            let short_hash = &full_hash[..hash_length.min(full_hash.len())];

            let stem = rel_path.file_stem().unwrap().to_str().unwrap();
            let ext = rel_path
                .extension()
                .map(|e| e.to_str().unwrap())
                .unwrap_or("");
            let hashed_name = hashed_filename(stem, short_hash, ext);

            let hashed_rel = match rel_path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    format!("{}/{hashed_name}", parent.to_string_lossy())
                }
                _ => hashed_name.clone(),
            };

            let dest = src_dir.join(&hashed_rel);
            if let Err(e) = fs::copy(&abs_path, &dest) {
                eprintln!(
                    "Warning: failed to copy '{}' to '{}': {e}",
                    abs_path.display(),
                    dest.display()
                );
                return caps[0].to_string();
            }

            manifest.insert(
                manifest_key,
                ManifestEntry {
                    hash: short_hash.to_string(),
                    hashed_path: hashed_rel,
                },
            );

            rewrite_asset_path(asset_path_str, &hashed_name)
        })
        .to_string();

    chapter.content = new_content;
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            c => parts.push(c),
        }
    }
    parts.iter().collect()
}

pub fn compute_hash(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Md5::new();
    hasher.update(&bytes);
    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

pub fn hashed_filename(stem: &str, hash: &str, ext: &str) -> String {
    if ext.is_empty() {
        format!("{stem}.{hash}")
    } else {
        format!("{stem}.{hash}.{ext}")
    }
}

pub fn rewrite_asset_path(original_path: &str, hashed_name: &str) -> String {
    if let Some(pos) = original_path.rfind('/') {
        format!("{}/{}", &original_path[..pos], hashed_name)
    } else {
        hashed_name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_matches_directive() {
        let text = "{{#asset-hash images/diagram.png}}";
        let caps = DIRECTIVE_RE.captures(text).unwrap();
        assert_eq!(caps[1].trim(), "images/diagram.png");
    }

    #[test]
    fn regex_matches_directive_with_extra_whitespace() {
        let text = "{{#asset-hash   images/photo.jpg  }}";
        let caps = DIRECTIVE_RE.captures(text).unwrap();
        assert_eq!(caps[1].trim(), "images/photo.jpg");
    }

    #[test]
    fn regex_no_match_for_non_directives() {
        let text = "![image](images/diagram.png)";
        assert!(DIRECTIVE_RE.captures(text).is_none());

        let text2 = "{{#include file.md}}";
        assert!(DIRECTIVE_RE.captures(text2).is_none());
    }

    #[test]
    fn test_hashed_filename_with_ext() {
        assert_eq!(
            hashed_filename("diagram", "a1b2c3d4", "png"),
            "diagram.a1b2c3d4.png"
        );
    }

    #[test]
    fn test_hashed_filename_without_ext() {
        assert_eq!(
            hashed_filename("LICENSE", "a1b2c3d4", ""),
            "LICENSE.a1b2c3d4"
        );
    }

    #[test]
    fn test_rewrite_asset_path_with_dir() {
        assert_eq!(
            rewrite_asset_path("images/diagram.png", "diagram.a1b2c3d4.png"),
            "images/diagram.a1b2c3d4.png"
        );
    }

    #[test]
    fn test_rewrite_asset_path_with_relative_dir() {
        assert_eq!(
            rewrite_asset_path("../images/foo.png", "foo.abcd1234.png"),
            "../images/foo.abcd1234.png"
        );
    }

    #[test]
    fn test_rewrite_asset_path_no_dir() {
        assert_eq!(
            rewrite_asset_path("diagram.png", "diagram.a1b2c3d4.png"),
            "diagram.a1b2c3d4.png"
        );
    }

    fn make_test_input(root: &Path, chapters: Vec<serde_json::Value>) -> String {
        let ctx = serde_json::json!({
            "root": root.to_string_lossy(),
            "config": {
                "book": {
                    "authors": ["Test"],
                    "language": "en",
                    "src": "src",
                    "title": "Test Book"
                },
                "preprocessor": {
                    "assets-hash": {}
                }
            },
            "renderer": "html",
            "mdbook_version": "0.5.1"
        });

        let book = serde_json::json!({
            "items": chapters,
            "__non_exhaustive": null
        });

        serde_json::to_string(&serde_json::json!([ctx, book])).unwrap()
    }

    fn make_chapter(name: &str, content: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "Chapter": {
                "name": name,
                "content": content,
                "number": [1],
                "sub_items": [],
                "path": path,
                "source_path": path,
                "parent_names": []
            }
        })
    }

    #[test]
    fn integration_basic_asset_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        let images_dir = src_dir.join("images");
        fs::create_dir_all(&images_dir).unwrap();

        fs::write(images_dir.join("diagram.png"), b"fake png content").unwrap();

        let chapter = make_chapter(
            "Chapter 1",
            "![diagram]({{#asset-hash images/diagram.png}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();

        let preprocessor = AssetsHash::new();
        let result = preprocessor.run(&ctx, book).unwrap();

        let ch = match result.iter().next().unwrap() {
            BookItem::Chapter(ch) => ch,
            _ => panic!("Expected a chapter"),
        };
        assert!(
            !ch.content.contains("{{#asset-hash"),
            "Directive should be replaced, got: {}",
            ch.content
        );
        assert!(
            ch.content.contains("images/diagram."),
            "Should contain hashed path, got: {}",
            ch.content
        );

        assert!(
            images_dir.join("diagram.png").exists(),
            "Original should still exist"
        );
        let hashed_entries: Vec<_> = fs::read_dir(&images_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("diagram.") && name != "diagram.png"
            })
            .collect();
        assert_eq!(hashed_entries.len(), 1, "Should have one hashed copy");

        let manifest_path = root.join("assets-manifest.json");
        assert!(manifest_path.exists(), "Manifest should exist");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(manifest.contains_key("images/diagram.png"));
        let entry = &manifest["images/diagram.png"];
        assert!(!entry.hash.is_empty());
        assert!(entry.hashed_path.starts_with("images/diagram."));
    }

    #[test]
    fn integration_idempotency() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        let images_dir = src_dir.join("images");
        fs::create_dir_all(&images_dir).unwrap();

        fs::write(images_dir.join("photo.jpg"), b"fake jpg content").unwrap();

        let chapter = make_chapter(
            "Chapter 1",
            "![photo]({{#asset-hash images/photo.jpg}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);

        // First run
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();
        let preprocessor = AssetsHash::new();
        let _ = preprocessor.run(&ctx, book).unwrap();

        // Second run
        let chapter = make_chapter(
            "Chapter 1",
            "![photo]({{#asset-hash images/photo.jpg}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();
        let result = preprocessor.run(&ctx, book).unwrap();

        let ch = match result.iter().next().unwrap() {
            BookItem::Chapter(ch) => ch,
            _ => panic!("Expected a chapter"),
        };
        assert!(!ch.content.contains("{{#asset-hash"));
        assert!(images_dir.join("photo.jpg").exists());

        let hashed_files: Vec<_> = fs::read_dir(&images_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("photo.") && name != "photo.jpg"
            })
            .collect();
        assert_eq!(
            hashed_files.len(),
            1,
            "Should have exactly one hashed copy after two runs"
        );
    }

    #[test]
    fn integration_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let chapter = make_chapter(
            "Chapter 1",
            "![missing]({{#asset-hash images/nonexistent.png}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();

        let preprocessor = AssetsHash::new();
        let result = preprocessor.run(&ctx, book).unwrap();

        let ch = match result.iter().next().unwrap() {
            BookItem::Chapter(ch) => ch,
            _ => panic!("Expected a chapter"),
        };
        assert!(
            ch.content
                .contains("{{#asset-hash images/nonexistent.png}}"),
            "Directive should be left unchanged for missing files, got: {}",
            ch.content
        );
    }

    #[test]
    fn integration_multiple_chapters_same_asset() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        let images_dir = src_dir.join("images");
        fs::create_dir_all(&images_dir).unwrap();

        fs::write(images_dir.join("shared.png"), b"shared content").unwrap();

        let ch1 = make_chapter(
            "Chapter 1",
            "![shared]({{#asset-hash images/shared.png}})",
            "chapter_1.md",
        );
        let ch2 = make_chapter(
            "Chapter 2",
            "![shared]({{#asset-hash images/shared.png}})",
            "chapter_2.md",
        );
        let input = make_test_input(root, vec![ch1, ch2]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();

        let preprocessor = AssetsHash::new();
        let result = preprocessor.run(&ctx, book).unwrap();

        let chapters: Vec<_> = result
            .iter()
            .filter_map(|item| match item {
                BookItem::Chapter(ch) => Some(ch),
                _ => None,
            })
            .collect();
        assert_eq!(chapters.len(), 2);
        for ch in &chapters {
            assert!(
                !ch.content.contains("{{#asset-hash"),
                "Directive should be replaced in {}, got: {}",
                ch.name,
                ch.content
            );
        }

        let manifest_path = root.join("assets-manifest.json");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.len(), 1);
        assert!(manifest.contains_key("images/shared.png"));

        assert!(images_dir.join("shared.png").exists());

        let hashed_files: Vec<_> = fs::read_dir(&images_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("shared.") && name != "shared.png"
            })
            .collect();
        assert_eq!(hashed_files.len(), 1);
    }

    #[test]
    fn integration_cleanup_removes_stale_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        let images_dir = src_dir.join("images");
        fs::create_dir_all(&images_dir).unwrap();

        fs::write(images_dir.join("logo.png"), b"version 1").unwrap();
        let chapter = make_chapter(
            "Chapter 1",
            "![logo]({{#asset-hash images/logo.png}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();
        let preprocessor = AssetsHash::new();
        let _ = preprocessor.run(&ctx, book).unwrap();

        let first_hashed: Vec<_> = fs::read_dir(&images_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("logo.") && name != "logo.png"
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(first_hashed.len(), 1);
        let old_hashed_name = &first_hashed[0];

        fs::write(images_dir.join("logo.png"), b"version 2").unwrap();

        let chapter = make_chapter(
            "Chapter 1",
            "![logo]({{#asset-hash images/logo.png}})",
            "chapter_1.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();
        let _ = preprocessor.run(&ctx, book).unwrap();

        assert!(!images_dir.join(old_hashed_name).exists());
        assert!(images_dir.join("logo.png").exists());

        let new_hashed: Vec<_> = fs::read_dir(&images_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("logo.") && name != "logo.png"
            })
            .collect();
        assert_eq!(
            new_hashed.len(),
            1,
            "Should have exactly one new hashed copy"
        );
    }

    #[test]
    fn integration_chapter_in_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src_dir = root.join("src");
        let subdir = src_dir.join("guide");
        let images_dir = subdir.join("images");
        fs::create_dir_all(&images_dir).unwrap();

        fs::write(images_dir.join("fig.png"), b"figure content").unwrap();

        let chapter = make_chapter(
            "Guide",
            "![fig]({{#asset-hash images/fig.png}})",
            "guide/intro.md",
        );
        let input = make_test_input(root, vec![chapter]);
        let (ctx, book) = mdbook_preprocessor::parse_input(input.as_bytes()).unwrap();

        let preprocessor = AssetsHash::new();
        let result = preprocessor.run(&ctx, book).unwrap();

        let ch = match result.iter().next().unwrap() {
            BookItem::Chapter(ch) => ch,
            _ => panic!("Expected a chapter"),
        };
        assert!(
            !ch.content.contains("{{#asset-hash"),
            "Directive should be replaced, got: {}",
            ch.content
        );
        assert!(
            ch.content.contains("images/fig."),
            "Should reference hashed file in images dir, got: {}",
            ch.content
        );

        let manifest_path = root.join("assets-manifest.json");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(
            manifest.contains_key("guide/images/fig.png"),
            "Manifest key should be relative to src dir, keys: {:?}",
            manifest.keys().collect::<Vec<_>>()
        );
    }
}
