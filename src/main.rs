#[macro_use]
extern crate clap;
extern crate irb;
extern crate las;
extern crate palette;
extern crate riscan_pro;
extern crate scanifc;
#[macro_use]
extern crate text_io;

use clap::{App, ArgMatches};
use irb::Irb;
use las::Color;
use las::point::Format;
use palette::{Gradient, Rgb};
use riscan_pro::{CameraCalibration, MountCalibration, Point, Project, ScanPosition, Socs};
use riscan_pro::scan_position::Image;
use scanifc::point3d::Stream;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::u16;

fn main() {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();
    print!("Configuring...");
    std::io::stdout().flush().unwrap();
    let config = Config::new(&matches);
    println!("done.");
    println!("{}", config);
    loop {
        print!("Continue? (y/n) ");
        std::io::stdout().flush().unwrap();
        let answer: String = read!();
        println!();
        match answer.to_lowercase().as_str() {
            "y" => break,
            "n" => return,
            _ => println!("Unknown response: {}", answer),
        }
    }

    for scan_position in config.scan_positions() {
        println!("Colorizing {}:", scan_position.name);
        let translations = config.translations(scan_position);
        if translations.is_empty() {
            println!("  - No translations found");
        } else {
            for translation in translations {
                println!(
                    "  - Translation:\n    - Infile: {}\n    - Outfile: {}",
                    translation.infile.display(),
                    translation.outfile.display()
                );
                config.colorize(scan_position, &translation);
            }
        }
    }
    println!("Complete!");
}

struct Config {
    image_dir: PathBuf,
    keep_without_thermal: bool,
    las_dir: PathBuf,
    max_reflectance: f32,
    min_reflectance: f32,
    project: Project,
    rotate: bool,
    scan_position_names: Option<Vec<String>>,
    sync_to_pps: bool,
    temperature_gradient: Gradient<Rgb>,
    use_scanpos_names: bool,
}

struct ImageGroup<'a> {
    camera_calibration: &'a CameraCalibration,
    image: &'a Image,
    irb: Irb,
    irb_path: PathBuf,
    mount_calibration: &'a MountCalibration,
    rotate: bool,
}

struct Translation {
    infile: PathBuf,
    outfile: PathBuf,
}

impl Config {
    fn new(matches: &ArgMatches) -> Config {
        let project = Project::from_path(matches.value_of("PROJECT").unwrap()).unwrap();
        let image_dir = PathBuf::from(matches.value_of("IMAGE_DIR").unwrap());
        let las_dir = Path::new(matches.value_of("LAS_DIR").unwrap()).to_path_buf();
        let min_reflectance = value_t!(matches, "min-reflectance", f32).unwrap();
        let max_reflectance = value_t!(matches, "max-reflectance", f32).unwrap();
        let min_temperature = value_t!(matches, "min-temperature", f32).unwrap();
        let max_temperature = value_t!(matches, "max-temperature", f32).unwrap();
        let min_temperature_color = Rgb::new(0.0, 0., 1.0);
        let max_temperature_color = Rgb::new(1.0, 0., 0.);
        let temperature_gradient = Gradient::with_domain(vec![
            (min_temperature, min_temperature_color),
            (max_temperature, max_temperature_color),
        ]);
        Config {
            image_dir: image_dir,
            keep_without_thermal: matches.is_present("keep-without-thermal"),
            las_dir: las_dir,
            max_reflectance: max_reflectance,
            min_reflectance: min_reflectance,
            project: project,
            rotate: matches.is_present("rotate"),
            scan_position_names: matches.values_of("scan-position").map(|values| {
                values.map(|name| name.to_string()).collect()
            }),
            sync_to_pps: matches.is_present("sync-to-pps"),
            temperature_gradient: temperature_gradient,
            use_scanpos_names: matches.is_present("use-scanpos-names"),
        }
    }

    fn translations(&self, scan_position: &ScanPosition) -> Vec<Translation> {
        let paths = scan_position.singlescan_rxp_paths(&self.project);
        if self.use_scanpos_names && paths.len() > 1 {
            panic!(
                "--use-scanpos-names was provided, but there are {} rxp files for scan position {}",
                paths.len(),
                scan_position.name
            );
        }
        paths
            .into_iter()
            .map(|path| {
                Translation {
                    outfile: self.outfile(scan_position, &path),
                    infile: path,
                }
            })
            .collect()
    }

    fn colorize(&self, scan_position: &ScanPosition, translation: &Translation) {
        use std::f64;

        let image_groups = self.image_groups(scan_position);
        let stream = Stream::from_path(&translation.infile)
            .sync_to_pps(self.sync_to_pps)
            .open()
            .unwrap();
        let mut writer = las::Writer::from_path(&translation.outfile, self.las_header()).unwrap();

        for point in stream {
            let point = point.expect("could not read rxp point");
            let socs = Point::socs(point.x, point.y, point.z);
            let temperatures = image_groups
                .iter()
                .filter_map(|image_group| image_group.temperature(&socs))
                .collect::<Vec<_>>();
            let temperature = if temperatures.is_empty() {
                if self.keep_without_thermal {
                    f64::NAN
                } else {
                    continue;
                }
            } else {
                temperatures.iter().sum::<f64>() / temperatures.len() as f64
            };
            let glcs = socs.to_prcs(scan_position.sop).to_glcs(self.project.pop);
            let point = las::Point {
                x: glcs.x,
                y: glcs.y,
                z: glcs.z,
                intensity: self.to_intensity(point.reflectance),
                color: Some(self.to_color(temperature as f32)),
                gps_time: Some(temperature),
                ..Default::default()
            };
            writer.write(point).expect("could not write las point");
        }
    }

    fn scan_positions(&self) -> Vec<&ScanPosition> {
        let mut scan_positions: Vec<_> = if let Some(names) = self.scan_position_names.as_ref() {
            names
                .iter()
                .map(|name| self.project.scan_positions.get(name).unwrap())
                .collect()
        } else {
            self.project.scan_positions.values().collect()
        };
        scan_positions.sort_by_key(|s| &s.name);
        scan_positions
    }

    fn to_color(&self, n: f32) -> Color {
        let color = self.temperature_gradient.get(n);
        Color {
            red: (u16::MAX as f32 * color.red) as u16,
            green: (u16::MAX as f32 * color.green) as u16,
            blue: (u16::MAX as f32 * color.blue) as u16,
        }
    }

    fn to_intensity(&self, n: f32) -> u16 {
        (u16::MAX as f32 * (n - self.min_reflectance) /
             (self.max_reflectance - self.min_reflectance)) as u16
    }

    fn las_header(&self) -> las::Header {
        let mut header = las::Header::default();
        header.point_format = Format::new(3).unwrap();
        header.transforms = las::Vector {
            x: las::Transform {
                scale: 0.001,
                offset: self.project.pop[(0, 3)],
            },
            y: las::Transform {
                scale: 0.001,
                offset: self.project.pop[(1, 3)],
            },
            z: las::Transform {
                scale: 0.001,
                offset: self.project.pop[(2, 3)],
            },
        };
        header
    }

    fn image_groups<'a>(&'a self, scan_position: &'a ScanPosition) -> Vec<ImageGroup<'a>> {
        let mut image_dir = self.image_dir.clone();
        image_dir.push(&scan_position.name);
        match fs::read_dir(image_dir) {
            Ok(read_dir) => {
                read_dir
                    .filter_map(|entry| {
                        let entry = entry.unwrap();
                        if entry.path().extension().map(|e| e == "irb").unwrap_or(
                            false,
                        )
                        {
                            let image = scan_position.image_from_path(entry.path()).unwrap();
                            let irb = Irb::from_path(entry.path().to_string_lossy().as_ref())
                                .unwrap();
                            let camera_calibration =
                                image.camera_calibration(&self.project).unwrap();
                            let mount_calibration = image.mount_calibration(&self.project).unwrap();
                            Some(ImageGroup {
                                camera_calibration: camera_calibration,
                                image: image,
                                irb: irb,
                                irb_path: entry.path(),
                                mount_calibration: mount_calibration,
                                rotate: self.rotate,
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            }
            Err(err) => {
                use std::io::ErrorKind;
                match err.kind() {
                    ErrorKind::NotFound => Vec::new(),
                    _ => panic!("io error: {}", err),
                }
            }
        }
    }

    fn outfile<P: AsRef<Path>>(&self, scan_position: &ScanPosition, infile: P) -> PathBuf {
        let mut outfile = self.las_dir.clone();
        if self.use_scanpos_names {
            outfile.push(Path::new(&scan_position.name).with_extension("las"));
        } else {
            outfile.push(infile.as_ref().with_extension("las").file_name().unwrap());
        }
        outfile
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Configuration:")?;
        writeln!(f, "  - project: {}", self.project.path.display())?;
        writeln!(f, "  - image dir: {}", self.image_dir.display())?;
        writeln!(f, "  - las dir: {}", self.las_dir.display())?;
        writeln!(f, "  - scan positions:")?;
        for scan_position in self.scan_positions() {
            writeln!(f, "    - name: {}", scan_position.name)?;
            let image_groups = self.image_groups(scan_position);
            if image_groups.is_empty() {
                writeln!(f, "    - no images for this scan position")?;
            } else {
                writeln!(f, "    - images:")?;
                for image_group in image_groups {
                    writeln!(f, "      - {}", image_group.irb_path.display())?;
                }
            }
        }

        Ok(())
    }
}

impl<'a> ImageGroup<'a> {
    fn temperature(&self, socs: &Point<Socs>) -> Option<f64> {
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
