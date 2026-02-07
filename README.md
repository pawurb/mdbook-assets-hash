# mdbook-assets-hash
[![Latest Version](https://img.shields.io/crates/v/mdbook-assets-hash.svg)](https://crates.io/crates/mdbook-assets-hash)

> ⚠️ Disclaimer  
> Mostly one-shotted with Claude Code. Works for me.

An [mdBook](https://rust-lang.github.io/mdBook/) preprocessor that adds content-based cache-busting hashes to asset filenames.

See [hotpath.rs](https://hotpath.rs) for a live demo. 

Assets are opted in explicitly using the `{{#asset-hash path}}` directive. The preprocessor computes an MD5 hash of each referenced file, copies it with a hashed filename (e.g., `image.png` → `image.a1b2c3d4.png`), replaces the directive with the hashed path, and writes an `assets-manifest.json` to the book root.

Works with any file type — images, PDFs, fonts, CSS, JS, etc.

## Installation

```sh
cargo install mdbook-assets-hash
```

Then place the binary (`target/release/mdbook-assets-hash`) somewhere on your `PATH`.

## Configuration

Add the preprocessor to your `book.toml`:

```toml
[preprocessor.assets-hash]
hash-length = 8  # optional, default 8
```

Since the binary follows the `mdbook-` naming convention, mdBook will discover it automatically. The `command` field is not required.

## Usage

Wrap any asset path in the `{{#asset-hash ...}}` directive. The directive expands to the hashed path string, so place it wherever a path is expected:

```markdown
![diagram]({{#asset-hash images/diagram.png}})

<img src="{{#asset-hash images/photo.jpg}}" alt="Photo">

[Download PDF]({{#asset-hash assets/guide.pdf}})
```

After preprocessing, this becomes:

```markdown
![diagram](images/diagram.a1b2c3d4.png)

<img src="images/photo.e5f6a7b8.jpg" alt="Photo">

[Download PDF](assets/guide.9c0d1e2f.pdf)
```

Assets **not** wrapped in the directive are left untouched.

## Asset paths

Paths inside the directive are resolved **relative to the chapter's markdown file**, just like standard mdBook links. For example, given this structure:

```
src/
  SUMMARY.md
  chapter_1.md
  guide/
    intro.md
    images/
      fig.png
```

In `guide/intro.md` you would write:

```markdown
![fig]({{#asset-hash images/fig.png}})
```

## Manifest

Each build produces an `assets-manifest.json` at the book root (next to `book.toml`):

```json
{
  "images/diagram.png": {
    "hash": "a1b2c3d4",
    "hashed_path": "images/diagram.a1b2c3d4.png"
  },
  "assets/guide.pdf": {
    "hash": "9c0d1e2f",
    "hashed_path": "assets/guide.9c0d1e2f.pdf"
  }
}

```

Keys are paths relative to the `src` directory. This file can be consumed by web servers or deployment scripts to set up redirects or long-lived cache headers.

