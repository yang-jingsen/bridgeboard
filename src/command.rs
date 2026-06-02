use std::process::Command;

pub fn quiet_command(program: &str) -> Command {
    let mut command = Command::new(program);
    apply_quiet_spawn(&mut command);
    command
}

pub fn apply_quiet_spawn(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}
