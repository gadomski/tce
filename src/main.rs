#[macro_use]
extern crate clap;
extern crate irb;
extern crate las;
extern crate palette;
extern crate riscan_pro;
extern crate scanifc;
extern crate walkdir;

use clap::App;
use irb::Irb;
use las::{Header, Writer};
use las::point::Color;
use palette::{Gradient, Rgb};
use riscan_pro::{CameraCalibration, MountCalibration, Point, Project, Socs};
use riscan_pro::scan_position::Image;
use scanifc::point3d::Stream;
use std::collections::BTreeMap;
use std::path::Path;
use std::process;
use std::u16;
use walkdir::WalkDir;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    // ## Project
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

    // ## Scan positions
    //
    // Inform the user if we're filtering which scan positions we're processing.
    if let Some(requested) = matches.values_of("scan-position") {
        println!("Only colorizing these scan positions:");
        for requested in requested {
            println!("  - {}", requested);
        }
    } else {
        println!("Colorizing all scan positions");
    }
    // Each scan position's name will be used to reference a vector of `ImageGroup`s.
    let mut scan_positions = BTreeMap::new();

    // # Images
    //
    // We assume all of the images have an "irb" extension, and that the image names can be used to
    // deduce the scan position name and image name, e.g. "ScanPos001 - Image001.irb".
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
            // If we have a scan position filter, apply it.
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
            let camera_calibration = match image.camera_calibration(&project) {
                Ok(camera_calibration) => camera_calibration,
                Err(_) => {
                    println!(
                        "Invalid project configuration, no camera calibration named {}",
                        image.camera_calibration_name
                    );
                    process::exit(1);
                }
            };
            let mount_calibration = match image.mount_calibration(&project) {
                Ok(mount_calibration) => mount_calibration,
                Err(_) => {
                    println!(
                        "Invalid project configuration, no mount calibration named {}",
                        image.mount_calibration_name
                    );
                    process::exit(1);
                }
            };

            // An `ImageGroup` collects all the stuff necessary to turn a socs point into a thermal
            // image temperature.
            scan_positions
                .entry(&scan_position.name)
                .or_insert_with(Vec::new)
                .push(ImageGroup {
                    camera_calibration: camera_calibration,
                    image: image,
                    irb: irb,
                    mount_calibration: mount_calibration,
                });
        }
    }

    println!("Found {} scan positions:", scan_positions.len());
    for (name, image_groups) in scan_positions.iter() {
        println!("  - {} with {} images", name, image_groups.len());
    }

    // ## Las setup
    //
    // All the output las files will get dropped in this one directory.
    let las_dir = Path::new(matches.value_of("LAS_DIR").unwrap()).to_path_buf();
    // The same header settings are used for each output file.
    let mut header = Header::default();
    // Las point format three includes gps time (which use use to store the temperature float) and
    // rgb data.
    header.point_format = 3.into();
    // Reflectance values need to be scaled to intensity u16s.
    let min_reflectance = value_t!(matches, "min-reflectance", f32).unwrap_or_else(|e| {
        println!("Unable to parse min reflectance as a f32: {}", e);
        process::exit(1)
    });
    let max_reflectance = value_t!(matches, "max-reflectance", f32).unwrap_or_else(|e| {
        println!("Unable to parse max reflectance as a f32: {}", e);
        process::exit(1)
    });
    let to_intensity =
        |n| (u16::MAX as f32 * (n - min_reflectance) / (max_reflectance - min_reflectance)) as u16;
    // Temperatures are mapped onto a color scale.
    let min_temperature = value_t!(matches, "min-temperature", f64).unwrap_or_else(|e| {
        println!("Unable to parse min temperature as a f64: {}", e);
        process::exit(1)
    });
    let max_temperature = value_t!(matches, "max-temperature", f64).unwrap_or_else(|e| {
        println!("Unable to parse min temperature as a f64: {}", e);
        process::exit(1)
    });
    let min_temperature_color = Rgb::new(0., 0., 1.0);
    let max_temperature_color = Rgb::new(1.0, 0., 0.);
    let temperature_gradient = Gradient::with_domain(vec![
        (min_temperature, min_temperature_color),
        (max_temperature, max_temperature_color),
    ]);
    let to_color = |n| {
        let color = temperature_gradient.get(n);
        Color {
            red: (u16::MAX as f64 * color.red) as u16,
            green: (u16::MAX as f64 * color.green) as u16,
            blue: (u16::MAX as f64 * color.blue) as u16,
        }
    };

    for (name, image_groups) in scan_positions.iter() {
        println!("Colorizing scan position: {}", name);

        let scan_position = project.scan_positions.get(name.as_str()).unwrap();
        // We colorize every singlescan rxp, as defined in the project xml.
        let paths = scan_position.singlescan_rxp_paths(&project);
        for rxpfile in paths {
            let stream = match Stream::from_path(&rxpfile)
                .sync_to_pps(matches.is_present("sync-to-pps"))
                .open() {
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

            for point in stream {
                let point = point.expect("could not read rxp point");
                let socs = Point::socs(point.x, point.y, point.z);
                let temperatures = image_groups
                    .iter()
                    .filter_map(|image_group| image_group.color(&socs))
                    .collect::<Vec<_>>();
                // If there are no temperatures, skip the point.
                if temperatures.is_empty() {
                    continue;
                }
                // Since multiple images might have a pixel for a given point, we average all the
                // temperatures. This is to avoid harsh lines as one image breaks into another.
                let temperature = temperatures.iter().sum::<f64>() / temperatures.len() as f64;
                let glcs = socs.to_prcs(scan_position.sop).to_glcs(project.pop);
                let point = las::Point {
                    x: glcs.x,
                    y: glcs.y,
                    z: glcs.z,
                    intensity: to_intensity(point.reflectance),
                    color: Some(to_color(temperature)),
                    gps_time: Some(temperature),
                    ..Default::default()
                };
                writer.write(&point).expect("could not write las point");
            }
        }
    }
    println!("Done!");
}

struct ImageGroup<'a> {
    camera_calibration: &'a CameraCalibration,
    image: &'a Image,
    irb: Irb,
    mount_calibration: &'a MountCalibration,
}

impl<'a> ImageGroup<'a> {
    fn color(&self, socs: &Point<Socs>) -> Option<f64> {
        let cmcs = socs.to_cmcs(self.image.cop, self.mount_calibration);
        self.camera_calibration.cmcs_to_ics(&cmcs).map(|(u, v)| {
            self.irb
                .temperature(u.trunc() as i32, v.trunc() as i32)
                .expect("error when retrieving temperature") - 273.15
        })
    }
}
