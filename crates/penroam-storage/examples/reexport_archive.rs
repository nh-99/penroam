//! Imports a `.penroam` archive into a temporary library and exports it again
//! with the current format (e.g. upgrades format 1 JSON pages to binary pages).
//!
//!     cargo run -p penroam-storage --example reexport_archive -- in.penroam out.penroam
use penroam_storage::Library;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (src, dst) = (&args[1], &args[2]);
    let dir = std::env::temp_dir().join(format!("penroam-reexport-{}", std::process::id()));
    let mut lib = Library::open(dir.join("library")).expect("library");
    let rep = lib
        .import_archive(std::fs::File::open(src).expect("open input"))
        .expect("import");
    let out = std::fs::File::create(dst).expect("create output");
    let m = lib
        .export_archive(rep.notebook.id, out, "reexport_archive")
        .expect("export");
    println!("{} pages, format {}", m.pages.len(), m.format_version);
    let _ = std::fs::remove_dir_all(dir);
}
