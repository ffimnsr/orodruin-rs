pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_HASH: &str = env!("ORODRUIN_GIT_HASH");
pub const GIT_DATE: &str = env!("ORODRUIN_GIT_DATE");
pub const BUILD_PROFILE: &str = env!("ORODRUIN_BUILD_PROFILE");
pub const BUILD_TARGET: &str = env!("ORODRUIN_BUILD_TARGET");

pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit: ",
    env!("ORODRUIN_GIT_HASH"),
    "\ndate: ",
    env!("ORODRUIN_GIT_DATE"),
    "\nbuild: ",
    env!("ORODRUIN_BUILD_PROFILE"),
    " (",
    env!("ORODRUIN_BUILD_TARGET"),
    ")"
);

pub fn render() -> String {
    format!(
        "orodruin {VERSION}\ncommit: {GIT_HASH}\ndate: {GIT_DATE}\nbuild: {BUILD_PROFILE} ({BUILD_TARGET})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_all_version_fields() {
        let rendered = render();
        assert!(rendered.contains(VERSION));
        assert!(rendered.contains(GIT_HASH));
        assert!(rendered.contains(GIT_DATE));
        assert!(rendered.contains(BUILD_PROFILE));
        assert!(rendered.contains(BUILD_TARGET));
    }
}
