use std::path::Path;
use std::process::Command;

pub type PlatformState = crate::sys::unix::UnixPlatformState;

pub fn get_command(exe_dir: &Path) -> (Command, String) {
    #[cfg(debug_assertions)]
    {
        let project_root = exe_dir.join("../../../..");
        let is_apple_silicon = Command::new("uname")
            .arg("-m")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "arm64")
            .unwrap_or(false);
        
        let mut c = if is_apple_silicon {
            let mut command = Command::new("/usr/bin/arch");
            command.arg("-arm64").arg("node");
            command
        } else {
            Command::new("node")
        };
        c.arg("--import").arg("tsx").arg("src/local.ts");
        c.current_dir(&project_root);
        (c, project_root.join("resource/migrate").to_string_lossy().into_owned())
    }
    
    #[cfg(not(debug_assertions))]
    {
        let sidecar_path = exe_dir.join("ai-gateway-backend");
        let resource_dir = exe_dir.join("../Resources/resource");
        let mut c = Command::new(&sidecar_path);
        c.arg("--api-only");
        c.arg("--desktop-mode");
        (c, resource_dir.join("migrate").to_string_lossy().into_owned())
    }
}

pub fn setup_command(cmd: &mut Command) -> PlatformState {
    crate::sys::unix::setup_pty_command(cmd)
}

pub fn post_spawn(state: &mut PlatformState, child: &mut std::process::Child) {
    crate::sys::unix::post_spawn(state, child)
}
