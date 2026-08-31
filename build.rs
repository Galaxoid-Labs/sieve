//! Compile bundled assets into the binary.
//!
//! The icons were loaded from the source tree through an icon search path
//! built from `CARGO_MANIFEST_DIR`. That works while the source is present and
//! nowhere else — not from a copied binary, and not inside a Flatpak, where
//! the path does not exist. Compiling them in means they travel with the
//! executable.

fn main() {
    glib_build_tools::compile_resources(&["data"], "data/sieve.gresource.xml", "sieve.gresource");
}
