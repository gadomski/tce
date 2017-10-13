extern crate ansi_term;
#[macro_use]
extern crate clap;
extern crate irb;
extern crate riscan_pro;
extern crate walkdir;

use ansi_term::Colour::Green;
use clap::App;
use irb::Irb;
use riscan_pro::Project;
use std::collections::BTreeMap;
use std::process;
use walkdir::WalkDir;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    let project = match Project::from_path(matches.value_of("PROJECT").unwrap()) {
        Ok(project) => {
            println!(
                "Opened project: {}",
                Green.paint(project.path.to_string_lossy())
            );
            project
        }
        Err(err) => {
            println!("Could not open project: {}", err);
            process::exit(1);
        }
    };
    let mut scan_positions = BTreeMap::new();

    for entry in WalkDir::new(matches.value_of("IMAGE_DIR").unwrap()) {
        let entry = entry.unwrap();
        if entry.path().extension().map(|e| e == "irb").unwrap_or(
            false,
        )
        {
            let scan_position = match project.scan_position_from_path(entry.path()) {
                Ok(scan_position) => scan_position,
                Err(_) => {
                    println!(
                        "Could not find scan position for path: {}",
                        entry.path().display()
                    );
                    process::exit(1);
                }
            };
            let image = match scan_position.image_from_path(entry.path()) {
                Ok(image) => image,
                Err(_) => {
                    println!(
                        "Could not find scan position image for path: {}",
                        entry.path().display()
                    );
                    process::exit(1);
                }
            };
            let irb = match Irb::from_path(entry.path().to_string_lossy().as_ref()) {
                Ok(image) => image,
                Err(err) => {
                    println!(
                        "Could not read irb image at {}: {}",
                        entry.path().display(),
                        err
                    );
                    process::exit(1);
                }
            };

            scan_positions
                .entry(&scan_position.name)
                .or_insert_with(Vec::new)
                .push((image, irb));
        }
    }
}
