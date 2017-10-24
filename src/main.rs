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
use las::{Header, Transform, Vector, Writer};
use las::point::Color;
use palette::{Gradient, Rgb};
use riscan_pro::{CameraCalibration, MountCalibration, Point, Project, Socs};
use riscan_pro::scan_position::Image;
use scanifc::point3d::Stream;
use std::collections::BTreeMap;
use std::path::Path;
use std::u16;
use walkdir::WalkDir;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    let project = Project::from_path(matches.value_of("PROJECT").unwrap()).unwrap();
    println!("Opened project: {}", project.path.display());

    if let Some(requested) = matches.values_of("scan-position") {
        println!("Only colorizing these scan positions:");
        for requested in requested {
            println!("  - {}", requested);
        }
    } else {
        println!("Colorizing all scan positions");
    }
    let mut scan_positions = BTreeMap::new();
    for entry in WalkDir::new(matches.value_of("IMAGE_DIR").unwrap()) {
        let entry = entry.unwrap();
        if entry.path().extension().map(|e| e == "irb").unwrap_or(
            false,
        )
        {
            let scan_position = project.scan_position_from_path(entry.path()).unwrap();
            // If we have a scan position filter, apply it.
            if let Some(mut requested) = matches.values_of("scan-position") {
                if requested.all(|r| r != scan_position.name) {
                    continue;
                }
            }
            let image = scan_position.image_from_path(entry.path()).unwrap();
            let irb = Irb::from_path(entry.path().to_string_lossy().as_ref()).unwrap();
            let camera_calibration = image.camera_calibration(&project).unwrap();
            let mount_calibration = image.mount_calibration(&project).unwrap();
            scan_positions
                .entry(&scan_position.name)
                .or_insert_with(Vec::new)
                .push(ImageGroup {
                    camera_calibration: camera_calibration,
                    image: image,
                    irb: irb,
                    mount_calibration: mount_calibration,
                    rotate: matches.is_present("rotate"),
                });
        }
    }

    println!("Found {} scan positions:", scan_positions.len());
    for (name, image_groups) in scan_positions.iter() {
        println!("  - {} with {} images", name, image_groups.len());
    }

    let las_dir = Path::new(matches.value_of("LAS_DIR").unwrap()).to_path_buf();
    let mut header = Header::default();
    header.point_format = 3.into();
    header.transforms = Vector {
        x: Transform {
            scale: 0.001,
            offset: project.pop[(0, 3)],
        },
        y: Transform {
            scale: 0.001,
            offset: project.pop[(1, 3)],
        },
        z: Transform {
            scale: 0.001,
            offset: project.pop[(2, 3)],
        },
    };
    let min_reflectance = value_t!(matches, "min-reflectance", f32).unwrap();
    let max_reflectance = value_t!(matches, "max-reflectance", f32).unwrap();
    let to_intensity =
        |n| (u16::MAX as f32 * (n - min_reflectance) / (max_reflectance - min_reflectance)) as u16;
    let min_temperature = value_t!(matches, "min-temperature", f64).unwrap();
    let max_temperature = value_t!(matches, "max-temperature", f64).unwrap();
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
        let paths = scan_position.singlescan_rxp_paths(&project);
        let use_scanpos_names = matches.is_present("use-scanpos-names");
        if use_scanpos_names && paths.len() > 1 {
            panic!(
                "--use-scanpos-names was provided, but there are {} rxp files for scan position {}",
                paths.len(),
                scan_position.name
            );
        }
        for rxpfile in paths {
            let stream = Stream::from_path(&rxpfile)
                .sync_to_pps(matches.is_present("sync-to-pps"))
                .open()
                .unwrap();
            println!("Opened rxp stream at {}", rxpfile.display());
            let mut lasfile = las_dir.clone();
            if use_scanpos_names {
                lasfile.push(Path::new(&scan_position.name).with_extension("las"));
            } else {
                lasfile.push(rxpfile.with_extension("las").file_name().unwrap());
            }
            let mut writer = Writer::from_path(&lasfile, header.clone()).unwrap();
            println!("Opened las file at {}", lasfile.display());

            for point in stream {
                let point = point.expect("could not read rxp point");
                let socs = Point::socs(point.x, point.y, point.z);
                let temperatures = image_groups
                    .iter()
                    .filter_map(|image_group| image_group.color(&socs))
                    .collect::<Vec<_>>();
                if temperatures.is_empty() {
                    continue;
                }
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
    rotate: bool,
}

impl<'a> ImageGroup<'a> {
    fn color(&self, socs: &Point<Socs>) -> Option<f64> {
        let cmcs = socs.to_cmcs(self.image.cop, self.mount_calibration);
        self.camera_calibration.cmcs_to_ics(&cmcs).map(|(mut u,
          mut v)| {
            if self.rotate {
                let new_u = self.camera_calibration.height as f64 - v;
                v = u;
                u = new_u;
            }
            self.irb
                .temperature(u.trunc() as i32, v.trunc() as i32)
                .expect("error when retrieving temperature") - 273.15
        })
    }
}
