# tce

Thermal Colorization Engine.

![TCE](https://upload.wikimedia.org/wikipedia/commons/thumb/d/d8/Trikloreten.svg/300px-Trikloreten.svg.png)

Combines InfraTec thermal imagery with Riegl point clouds.
Built on:

- [riscan-pro](https://github.com/gadomski/riscan-pro)
- [irb-rs](https://github.com/gadomski/irb-rs)
- [las-rs](https://github.com/gadomski/las-rs)
- [rivlib-rs](https://github.com/gadomski/rivlib-rs)

...and numerous community repositories.

## Installation

You'll need [RiVLib](http://www.riegl.com/index.php?id=224) and InfraTec's irbacs library, which are only available from the vendors.
If you have those:

```bash
cargo install --git https://github.com/gadomski/tce
```

## Usage

Let the executable tell you:

```bash
tce --help
```
