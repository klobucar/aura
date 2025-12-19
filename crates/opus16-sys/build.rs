fn main() {
    // Require libopus >= 1.6 for DRED and 24-bit API support
    pkg_config::Config::new()
        .atleast_version("1.6")
        .probe("opus")
        .expect("libopus >= 1.6 required. Install with: brew install opus");
}
