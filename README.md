# `memalert`
`memalert` displays (Linux) desktop notifications warning of low system memory.

## Installation
```
# TODO Publish to crates.io
git clone https://github.com/andrewlabors/memalert && cargo install --path ./memalert
```

## Usage
`setsid --fork memalert`

A desktop notification will be displayed if free system memory falls below 33% of total system memory.

![`memalert` demo](memalert.png)

## License
MIT License, see <a href="LICENSE">LICENSE</a> file
