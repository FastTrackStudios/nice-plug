//! Build script for nih_plug_dioxus
//!
//! This script compiles Tailwind CSS at build time using the tailwindcss CLI.
//! If the CLI is not available, it falls back to a pre-compiled CSS file.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = PathBuf::from(&out_dir);

    let input_css = manifest_path.join("tailwind.css");
    let output_css = out_path.join("tailwind.compiled.css");

    // Tell Cargo to rerun if these files change
    println!("cargo:rerun-if-changed=tailwind.css");
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=examples/");

    // Also watch lumen-blocks for class changes
    let lumen_blocks_path = manifest_path.join("../../lumen-blocks/blocks/src");
    if lumen_blocks_path.exists() {
        println!(
            "cargo:rerun-if-changed={}",
            lumen_blocks_path.to_string_lossy()
        );
    }

    // Try to find tailwindcss CLI
    let tailwind_binary = find_tailwind_binary();

    if let Some(binary) = tailwind_binary {
        println!("cargo:warning=Using Tailwind CSS CLI: {}", binary.display());

        let status = Command::new(&binary)
            .arg("--input")
            .arg(&input_css)
            .arg("--output")
            .arg(&output_css)
            .current_dir(&manifest_path)
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("cargo:warning=Tailwind CSS compiled successfully");
                // NOTE: Testing if @layer works natively in Blitz/Stylo
                // Stylo 0.10.0 does have LayerOrder support, so this might not be needed
                // strip_css_layers(&output_css);
            }
            Ok(s) => {
                println!(
                    "cargo:warning=Tailwind CSS compilation failed with status: {}",
                    s
                );
                use_fallback_css(&output_css, &manifest_path);
            }
            Err(e) => {
                println!("cargo:warning=Failed to run Tailwind CSS CLI: {}", e);
                use_fallback_css(&output_css, &manifest_path);
            }
        }
    } else {
        println!("cargo:warning=Tailwind CSS CLI not found, using fallback CSS");
        use_fallback_css(&output_css, &manifest_path);
    }
}

/// Find the tailwindcss binary in common locations
fn find_tailwind_binary() -> Option<PathBuf> {
    // Check if tailwindcss is in PATH
    if let Ok(output) = Command::new("which").arg("tailwindcss").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    // Check common installation locations
    let home = env::var("HOME").ok()?;

    // Dioxus CLI installs tailwindcss here (with version prefix)
    let dioxus_tailwind = PathBuf::from(&home)
        .join(".local/share/dioxus/tailwind")
        .join(format!("tailwindcss-v4.1.5{}", binary_suffix()));
    if dioxus_tailwind.exists() {
        return Some(dioxus_tailwind);
    }

    // Also check for platform-specific binary name (no version prefix)
    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        let platform_binary = "tailwindcss-macos-arm64";
        #[cfg(target_arch = "x86_64")]
        let platform_binary = "tailwindcss-macos-x64";

        let dioxus_tailwind_platform = PathBuf::from(&home)
            .join(".local/share/dioxus/tailwind")
            .join(platform_binary);
        if dioxus_tailwind_platform.exists() {
            return Some(dioxus_tailwind_platform);
        }
    }

    // Also check for any version in dioxus tailwind dir
    let dioxus_tailwind_dir = PathBuf::from(&home).join(".local/share/dioxus/tailwind");
    if dioxus_tailwind_dir.exists() {
        if let Ok(entries) = fs::read_dir(&dioxus_tailwind_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = path.file_name().unwrap().to_string_lossy();
                    if name.starts_with("tailwindcss") {
                        return Some(path);
                    }
                }
            }
        }
    }

    // npm global install
    let npm_global = PathBuf::from(&home)
        .join(".npm/bin")
        .join(format!("tailwindcss{}", binary_suffix()));
    if npm_global.exists() {
        return Some(npm_global);
    }

    // Homebrew on macOS
    #[cfg(target_os = "macos")]
    {
        let brew_path = PathBuf::from("/opt/homebrew/bin/tailwindcss");
        if brew_path.exists() {
            return Some(brew_path);
        }
        let brew_path_intel = PathBuf::from("/usr/local/bin/tailwindcss");
        if brew_path_intel.exists() {
            return Some(brew_path_intel);
        }
    }

    None
}

fn binary_suffix() -> &'static str {
    #[cfg(windows)]
    return ".exe";
    #[cfg(not(windows))]
    return "";
}

/// Strip @layer wrappers from CSS since Blitz/Stylo doesn't fully support them.
/// This converts `@layer foo { .class { ... } }` to just `.class { ... }`
#[allow(dead_code)]
fn strip_css_layers(css_path: &Path) {
    let Ok(css) = fs::read_to_string(css_path) else {
        return;
    };

    let mut result = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    let mut in_layer = false;
    let mut brace_depth = 0;

    while let Some(c) = chars.next() {
        // Check for @layer
        if c == '@' {
            // Peek ahead to see if it's @layer
            let mut peek_str = String::new();
            let mut temp_chars = chars.clone();
            for _ in 0..5 {
                if let Some(pc) = temp_chars.next() {
                    peek_str.push(pc);
                }
            }

            if peek_str.starts_with("layer") {
                // Skip @layer and its name until we hit '{'
                for _ in 0..5 {
                    chars.next(); // consume "layer"
                }
                // Skip whitespace and layer name until '{'
                while let Some(&nc) = chars.peek() {
                    if nc == '{' {
                        chars.next(); // consume the '{'
                        in_layer = true;
                        brace_depth = 1;
                        break;
                    } else if nc == ';' {
                        // @layer declaration without block (e.g., @layer properties;)
                        chars.next();
                        break;
                    }
                    chars.next();
                }
                continue;
            }
        }

        if in_layer {
            if c == '{' {
                brace_depth += 1;
                result.push(c);
            } else if c == '}' {
                brace_depth -= 1;
                if brace_depth == 0 {
                    // End of @layer block, don't output the closing brace
                    in_layer = false;
                } else {
                    result.push(c);
                }
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }

    // Write the processed CSS back
    if let Err(e) = fs::write(css_path, result) {
        println!("cargo:warning=Failed to write processed CSS: {}", e);
    } else {
        println!("cargo:warning=Stripped @layer wrappers from CSS for Blitz compatibility");
    }
}

/// Use fallback CSS when Tailwind CLI is not available
fn use_fallback_css(output_css: &Path, manifest_path: &Path) {
    // Try to use pre-compiled CSS from lumen-blocks first
    let lumen_tailwind = manifest_path.join("../../lumen-blocks/docsite/assets/tailwind.css");
    if lumen_tailwind.exists() {
        if let Ok(css) = fs::read_to_string(&lumen_tailwind) {
            // Append our dark theme overrides
            let dark_theme = r#"
/* Dark theme activation via .dark class */
.dark,
:root.dark,
[data-theme="dark"] {
    --background: oklch(0.145 0 0);
    --foreground: oklch(0.985 0 0);
    --card: oklch(0.205 0 0);
    --card-foreground: oklch(0.985 0 0);
    --popover: oklch(0.145 0 0);
    --popover-foreground: oklch(0.985 0 0);
    --primary: oklch(0.985 0 0);
    --primary-foreground: oklch(0.205 0 0);
    --secondary: oklch(0.269 0 0);
    --secondary-foreground: oklch(0.985 0 0);
    --muted: oklch(0.269 0 0);
    --muted-foreground: oklch(0.708 0 0);
    --accent: oklch(0.269 0 0);
    --accent-foreground: oklch(0.985 0 0);
    --destructive: oklch(0.5058 0.2066 27.85);
    --border: oklch(0.269 0 0);
    --input: oklch(0.269 0 0);
    --ring: oklch(0.439 0 0);
}
"#;
            let combined = format!("{}\n{}", css, dark_theme);
            fs::write(output_css, combined).expect("Failed to write fallback CSS");
            return;
        }
    }

    // Last resort: use the theme.css from assets
    let theme_css = manifest_path.join("assets/theme.css");
    if theme_css.exists() {
        fs::copy(&theme_css, output_css).expect("Failed to copy fallback CSS");
    } else {
        // Create a minimal fallback
        let minimal_css = r#"
/* Minimal fallback CSS */
:root {
    --background: oklch(1 0 0);
    --foreground: oklch(0.145 0 0);
    --primary: oklch(0.205 0 0);
    --primary-foreground: oklch(0.985 0 0);
    --secondary: oklch(0.97 0 0);
    --secondary-foreground: oklch(0.205 0 0);
    --muted: oklch(0.97 0 0);
    --muted-foreground: oklch(0.556 0 0);
    --accent: oklch(0.97 0 0);
    --accent-foreground: oklch(0.205 0 0);
    --destructive: oklch(0.577 0.245 27.325);
    --border: oklch(0.922 0 0);
    --input: oklch(0.922 0 0);
    --ring: oklch(0.708 0 0);
    --card: oklch(1 0 0);
    --card-foreground: oklch(0.145 0 0);
    --radius: 0.625rem;
}

.dark {
    --background: oklch(0.145 0 0);
    --foreground: oklch(0.985 0 0);
    --primary: oklch(0.985 0 0);
    --primary-foreground: oklch(0.205 0 0);
    --card: oklch(0.205 0 0);
    --card-foreground: oklch(0.985 0 0);
    --secondary: oklch(0.269 0 0);
    --secondary-foreground: oklch(0.985 0 0);
    --muted: oklch(0.269 0 0);
    --muted-foreground: oklch(0.708 0 0);
    --accent: oklch(0.269 0 0);
    --accent-foreground: oklch(0.985 0 0);
    --border: oklch(0.269 0 0);
    --input: oklch(0.269 0 0);
}

*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
html, body { font-family: system-ui, sans-serif; background: var(--background); color: var(--foreground); }
"#;
        fs::write(output_css, minimal_css).expect("Failed to write minimal fallback CSS");
    }
}
