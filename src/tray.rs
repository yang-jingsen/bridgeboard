use crate::status::{render_table, StatusRow};
use anyhow::Result;
use std::thread;
use std::time::Duration;

pub fn run_tray_loop<F>(interval_sec: u64, mut snapshot: F) -> Result<()>
where
    F: FnMut() -> Result<Vec<StatusRow>>,
{
    eprintln!(
        "bridgeboard tray runtime started. v1 uses the shared action/status core; \
         platform tray adapters can attach here without changing config or lifecycle logic."
    );
    loop {
        let rows = snapshot()?;
        eprint!("\x1b[2J\x1b[H");
        eprintln!("Bridgeboard tray status (Ctrl+C to quit)\n");
        eprintln!("{}", render_table(&rows));
        thread::sleep(Duration::from_secs(interval_sec.max(2)));
    }
}
