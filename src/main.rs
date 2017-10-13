#[macro_use]
extern crate clap;
extern crate irb;
extern crate las;
extern crate riscan_pro;
extern crate scanifc;
extern crate walkdir;

use clap::App;
use irb::Irb;
use las::{Header, Writer};
use riscan_pro::{Point, Project};
use scanifc::point3d::Stream;
use std::collections::BTreeMap;
use std::path::Path;
use std::process;
use walkdir::WalkDir;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();
    let las_dir = Path::new(matches.value_of("LAS_DIR").unwrap()).to_path_buf();
    let mut header = Header::default();
    header.point_format = 3.into();
    if let Some(requested) = matches.values_of("scan-position") {
        println!("Only colorizing these scan positions:");
        for requested in requested {
            println!("  - {}", requested);
        }
    } else {
        println!("Colorizing all scan positions");
    }

    let project = match Project::from_path(matches.value_of("PROJECT").unwrap()) {
        Ok(project) => {
            println!("Opened project: {}", project.path.to_string_lossy());
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
            if let Some(mut requested) = matches.values_of("scan-position") {
                if requested.all(|r| r != scan_position.name) {
                    continue;
                }
            }
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

    println!("Found {} scan positions:", scan_positions.len());
    for (name, entry) in scan_positions.iter() {
        println!("  - {} with {} images", name, entry.len());
    }

    for (name, image_pair) in scan_positions.iter() {
        println!("Colorizing scan position: {}", name);

        let scan_position = project.scan_positions.get(name.as_str()).unwrap();
        let paths = scan_position.singlescan_rxp_paths(&project);
        for rxpfile in paths {
            let stream = match Stream::from_path(&rxpfile).sync_to_pps(false).open() {
                Ok(stream) => {
                    println!("Opened rxp stream at {}", rxpfile.display());
                    stream
                }
                Err(err) => {
                    println!(
                        "Unable to open rxp stream at {}: {}",
                        rxpfile.display(),
                        err
                    );
                    process::exit(1);
                }
            };
            let mut lasfile = las_dir.clone();
            lasfile.push(rxpfile.with_extension("las").file_name().expect(
                "rxp path should have a file name",
            ));
            let mut writer = match Writer::from_path(&lasfile, header.clone()) {
                Ok(writer) => {
                    println!("Opened las file at {}", lasfile.display());
                    writer
                }
                Err(err) => {
                    println!("Could not open las file at {}: {}", lasfile.display(), err);
                    process::exit(1);
                }
            };
            unimplemented!()
        }
    }
}
