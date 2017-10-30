// The Thermal Colorization Engine.
//
// This is a single executable that colorizes point clouds with thermal imagery data.
// Specifically, it takes a [Riegl](http://www.riegl.com/) [RiSCAN
// Pro](http://www.riegl.com/products/software-packages/riscan-pro/) project, a directory of
// [InfraTec](http://www.infratec.eu/) imagery, and colorizes the scans in the RiSCAN Pro project
// with the InfraTec imagery. This won't Just Work — the RiSCAN Pro project needs to be set up a
// certain way, and the InfraTec imagery must be named in a way that the images can be linked back
// to their scan positions. Since this is a one-off project, you'll have to read this source (and
// the source of [riscan-pro](https://github.com/gadomski/riscan-pro) to figure out what that
// setup looks like).
//
// The source for **tce** is commented in the literate programming style, and the
// [docco](https://github.com/jashkenas/docco) output is available at
// <https://gadomski.github.io/tce>.

// [Clap](https://github.com/kbknapp/clap-rs) is our command-line argument parser.
#[macro_use]
extern crate clap;
// [irb](https://github.com/gadomski/irb-rs) reads data from InfraTec thermal imagery.
extern crate irb;
// [las](https://github.com/gadomski/las-rs) reads and writes
// [las](https://www.asprs.org/committee-general/laser-las-file-format-exchange-activities.html)
// point cloud data. We use it in this executable to write data only.
extern crate las;
// We use [palette](https://github.com/Ogeon/palette) to transform temperature values in to RGB
// colors, which are then applied to the output las points.
extern crate palette;
// The [riscan-pro](https://github.com/gadomski/riscan-pro) crate reads RiSCAN Pro xml files.
extern crate riscan_pro;
// [scanifc](https://github.com/gadomski/rivlib-rs) reads data from Riegl rxp files.
extern crate scanifc;
#[macro_use]
extern crate text_io;

// We bring in various names to make their later usages less verbose.

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

// The main function does all of the work in our executable.
fn main() {
    // Our command-line configuration is stored in cli.yml, to keep this main code file less noisy.
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();
    print!("Configuring...");
    // `print!`, on its own, doesn't flush stdout, so we have to manually flush to ensure our
    // "Configuring..." text is printed.
    std::io::stdout().flush().unwrap();
    // Creates a `Config` object from the command-line switches.
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

    // The user can specify which scan positions are to be processed via the command line, or by
    // default we process them all.
    //
    // We could have put all of this work into its own function, e.g. `Config::run`, but we break
    // it apart so we can print useful information to the user without littering our actual
    // business logic with lots of `println!` statements.
    for scan_position in config.scan_positions() {
        println!("Colorizing {}:", scan_position.name);
        // A translation is simply a infile->outfile map (see `Translation`).
        let translations = config.translations(scan_position);
        // The translation might be empty if there is no imagery available *and* the user only
        // asked for temperature-attributed points. It also might be empty if there are no scans in
        // the scan position.
        if translations.is_empty() {
            println!("  - No translations found");
        } else {
            for translation in translations {
                println!(
                    "  - Translation:\n    - Infile: {}\n    - Outfile: {}",
                    translation.infile.display(),
                    translation.outfile.display()
                );
                // This is where the actual colorization takes place.
                config.colorize(scan_position, &translation);
            }
        }
    }
    println!("Complete!");
}

// Essentially a map of our command-line options onto Rust types, with some processing.
struct Config {
    // The directory that will be searched for thermal imagery.
    image_dir: PathBuf,
    // Should points without thermal data be written to the output?
    keep_without_thermal: bool,
    // The directory that will hold all output files.
    las_dir: PathBuf,
    // The maximum reflectance value, used when scaling reflectance values to intensity values.
    max_reflectance: f32,
    // The minimum reflectance value, used when scaling reflectance values to intensity values.
    min_reflectance: f32,
    // The active `riscan_pro::Project`.
    project: Project,
    // Should the thermal images be rotated 90°? Some of our projects need this option.
    rotate: bool,
    // A list of scan position names to process. If None, all scan position names from the project
    // will be processed.
    scan_position_names: Option<Vec<String>>,
    // When reading rxp data, should we only read points that have been synced to an external pps
    // signal? If the data were collected without a GNSS, you probably want sync_to_pps to be
    // false.
    sync_to_pps: bool,
    // The gradient used to map temperate values onto rgb colors.
    temperature_gradient: Gradient<Rgb>,
    // Should output las files be named after their scan position (true) or from the source rxp
    // (false). Note that the engine will fail if this is true but there are more than one scan per
    // scan position.
    use_scanpos_names: bool,
}

// All the bits and parts necessary to lookup a temperature value for a given point in the
// Scanner's Own Coordinate System (SOCS).
struct ImageGroup<'a> {
    camera_calibration: &'a CameraCalibration,
    image: &'a Image,
    irb: Irb,
    irb_path: PathBuf,
    mount_calibration: &'a MountCalibration,
    rotate: bool,
}

// A simple infile->outfile map.
struct Translation {
    infile: PathBuf,
    outfile: PathBuf,
}

impl Config {
    // Creates a new `Config` from the command-line arguments.
    fn new(matches: &ArgMatches) -> Config {
        // Here, and elsewhere, we `unwrap` errors instead of handling them gracefully. If/when
        // this executable matures, we might handle these errors instead of just unwrapping them.
        let project = Project::from_path(matches.value_of("PROJECT").unwrap()).unwrap();
        let image_dir = PathBuf::from(matches.value_of("IMAGE_DIR").unwrap());
        let las_dir = Path::new(matches.value_of("LAS_DIR").unwrap()).to_path_buf();
        let min_reflectance = value_t!(matches, "min-reflectance", f32).unwrap();
        let max_reflectance = value_t!(matches, "max-reflectance", f32).unwrap();
        let min_temperature = value_t!(matches, "min-temperature", f32).unwrap();
        let max_temperature = value_t!(matches, "max-temperature", f32).unwrap();
        // Blue
        let min_temperature_color = Rgb::new(0.0, 0., 1.0);
        // Red
        let max_temperature_color = Rgb::new(1.0, 0., 0.);
        // Creates a gradient whose domain goes from min_temperature->max_temperature, and range
        // goes from blue->red.
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

    // Returns all translation for the scan position.
    fn translations(&self, scan_position: &ScanPosition) -> Vec<Translation> {
        // The rxp paths are extracted from the RiSCAN Pro xml file.
        let paths = scan_position.singlescan_rxp_paths(&self.project);
        // If we've asked to use the scan position name as the output file name, but there are more
        // than one scan for this scan position, panic. This should eventually be turned into an
        // error.
        if self.use_scanpos_names && paths.len() > 1 {
            panic!(
                "--use-scanpos-names was provided, but there are {} rxp files for scan position {}",
                paths.len(),
                scan_position.name
            );
        }
        // Convert the vector of paths into a vector of translations.
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

    // Colorize all the points in an infile, and write them out to an outfile.
    fn colorize(&self, scan_position: &ScanPosition, translation: &Translation) {
        use std::f64;

        // Extract all the images that can be used to colorize points in this scan position.
        let image_groups = self.image_groups(scan_position);
        // Open the rxp file.
        let stream = Stream::from_path(&translation.infile)
            .sync_to_pps(self.sync_to_pps)
            .open()
            .unwrap();
        // Open the output las file.
        let mut writer = las::Writer::from_path(&translation.outfile, self.las_header()).unwrap();

        // Read each point.
        for point in stream {
            let point = point.expect("could not read rxp point");
            let socs = Point::socs(point.x, point.y, point.z);
            // Compute all temperatures for this point. Because there is image overlap, a single
            // point might have zero, one, or more temperatures.
            let temperatures = image_groups
                .iter()
                .filter_map(|image_group| image_group.temperature(&socs))
                .collect::<Vec<_>>();
            let temperature = if temperatures.is_empty() {
                // If there are no temperatures, but we've asked to keep points without thermal
                // information, set the temperature to NaN.
                if self.keep_without_thermal {
                    f64::NAN
                } else {
                    // Otherwise, go to the next point in the rxp stream without writing a point to the
                    // las file.
                    continue;
                }
            } else {
                // Average all of the temperatures to get a single value.
                temperatures.iter().sum::<f64>() / temperatures.len() as f64
            };
            // Convert the socs point to a global point (GLCS).
            let glcs = socs.to_prcs(scan_position.sop).to_glcs(self.project.pop);
            // Create the las point.
            let point = las::Point {
                x: glcs.x,
                y: glcs.y,
                z: glcs.z,
                // Las intensity values only go from 0 to 65535, so we need to scale our
                // floating-point reflectance value to an intensity value.
                intensity: self.to_intensity(point.reflectance),
                // Looks up the color for the temperature. NAN goes to black.
                color: Some(self.to_color(temperature as f32)),
                // Sets the gps_time field to the temperature value.
                gps_time: Some(temperature),
                // We don't really care about the rest of the point attributes.
                ..Default::default()
            };
            // Writes the las point out to the outfile.
            writer.write(&point).expect("could not write las point");
            // las::Writer implements `Drop`, meaning that the las header gets rewritten with the
            // correct values when `writer` goes out of scope.
        }
    }

    // Returns all scan positions, as determined by (a) the names provided on the command line or
    // (b) all scan positions in the project, if none were specified.
    fn scan_positions(&self) -> Vec<&ScanPosition> {
        let mut scan_positions: Vec<_> = if let Some(names) = self.scan_position_names.as_ref() {
            names
                .iter()
                .map(|name| self.project.scan_positions.get(name).unwrap())
                .collect()
        } else {
            self.project.scan_positions.values().collect()
        };
        // Sorts the scan positions to ensure they're processed in a reasonable order, instead of
        // pseudo-randomly.
        scan_positions.sort_by_key(|s| &s.name);
        scan_positions
    }

    // Converts a temperature value to a color.
    fn to_color(&self, n: f32) -> Color {
        let color = self.temperature_gradient.get(n);
        Color {
            // The gradient returns color values from 0 to 1, floating point, so they need to be
            // scaled to the u16 values expected by the las format.
            red: (u16::MAX as f32 * color.red) as u16,
            green: (u16::MAX as f32 * color.green) as u16,
            blue: (u16::MAX as f32 * color.blue) as u16,
        }
    }

    // Scales a floating-point reflectance value to a integer intensity value.
    fn to_intensity(&self, n: f32) -> u16 {
        (u16::MAX as f32 * (n - self.min_reflectance) /
             (self.max_reflectance - self.min_reflectance)) as u16
    }

    // Creates the las header for our output files.
    fn las_header(&self) -> las::Header {
        let mut header = las::Header::default();
        // Point format 3 includes both gps time and color.
        header.point_format = Format::new(3).unwrap();
        header.transforms = las::Vector {
            x: las::Transform {
                scale: 0.001,
                // The project's POP works well as a scale factor.
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

    // Returns all images (with associated calibration structures) for the provided scan position.
    fn image_groups<'a>(&'a self, scan_position: &'a ScanPosition) -> Vec<ImageGroup<'a>> {
        let mut image_dir = self.image_dir.clone();
        // We assume that images are stored under the root image dir with a path: `<root image
        // dir>/<scan position name>/<image name>.irb`.
        image_dir.push(&scan_position.name);
        match fs::read_dir(image_dir) {
            Ok(read_dir) => {
                read_dir
                    // Use filter_map to both (a) discard any files that we don't like and (b)
                    // convert file paths to `ImageGroup`s.
                    .filter_map(|entry| {
                        let entry = entry.unwrap();
                        // Only trust files with an "irb" extension.
                        if entry.path().extension().map(|e| e == "irb").unwrap_or(
                            false,
                        )
                        {
                            // The **riscan-pro** crate contains some logic to deduce scan position
                            // images from file paths, and we trust that logic here.
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
                    // If the directory didn't exist, we just assume that there are no images for
                    // this scan position and carry on.
                    ErrorKind::NotFound => Vec::new(),
                    _ => panic!("io error: {}", err),
                }
            }
        }
    }

    // Computes the output las file for the provided scan position, infile, and the configured las
    // directory.
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
    // This is the import bits, where a SOCS point is converted into a temperature.
    fn temperature(&self, socs: &Point<Socs>) -> Option<f64> {
        // There is always a valid CMCS (CaMera's Coordiante System) point for a given SOCS point.
        let cmcs = socs.to_cmcs(self.image.cop, self.mount_calibration);
        // A CMCS point cannot always be mapped onto a pixel, e.g.:
        //
        // 1. The CMCS point is behind the camera.
        // 2. The CMCS point is outside of the angle masks, as specified in the RiSCAN Pro project
        //    configuration.
        // 3. The computed pixel coordinates are outside of the bounds of the image.
        self.camera_calibration.cmcs_to_ics(&cmcs).map(|(mut u,
          mut v)| {
            // A 90° rotation.
            if self.rotate {
                let new_u = self.camera_calibration.height as f64 - v;
                v = u;
                u = new_u;
            }
            // Look up the pixel in the image to get the temperature in Kelvin.
            self.irb
                .temperature(u.trunc() as i32, v.trunc() as i32)
                // Convert Kelvin to Celsius.
                .expect("error when retrieving temperature") - 273.15
        })
    }
}
// Fin.
