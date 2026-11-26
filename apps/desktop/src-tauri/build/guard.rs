// Shared by build.rs and by the crate's tests (`include!`).

/// Whether this build may contain the development bridge: only a build that is not a
/// release build. The bridge gives any local program that learns its token the window's
/// commands, so it must not exist in a program that is shipped.
#[allow(dead_code)]
fn bridge_allowed(profile: &str, bridge_feature: bool) -> Result<(), String> {
    if bridge_feature && profile != "debug" {
        return Err(format!(
            "the `dev-bridge` feature cannot be built with the `{profile}` profile: the \
             development bridge is for building and testing the interface and is never \
             shipped. Build without `--features dev-bridge`."
        ));
    }
    Ok(())
}
