# Bōca

Bōca, from the Old English genitive plural word for _books_, is a lightweight, batteries-included markdown preview tool written in rust.


Bōca is built atop [markdown-rs](https://github.com/wooorm/markdown-rs) and [axum](https://github.com/tokio-rs/axum) with a bit of [HTMX](https://htmx.org/) glue.


## Install

```bash
cargo install --path=./
```

## Usage

```bash
# Just run
boca readme.md
# Run in dark mode
boca --dark readme.md
# Supply a custom stylesheet
boca -s local.css readme.md
```
