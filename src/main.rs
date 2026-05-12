use sysinfo::{
    Components, Disks, Networks, System
};

use std::{io::{self, Write}, thread, time::Duration};

fn make_bar(process: f32) -> String {
    let mut progress_bar = String::from("[..........]");

    let filted_len = (process / 10.0).round() as usize;
    let filted_len = filted_len.min(10);

    if filted_len > 0 {
        progress_bar.replace_range(1..1 + filted_len, &"#".repeat(filted_len));
    }

    progress_bar
}

fn main () {
    let mut sys = System::new();
    let delay = Duration::from_secs(1);

    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let cpus = sys.cpus();
        let num_cpus = cpus.len();

        for (i, cpu) in cpus.iter().enumerate() {
            let bar = make_bar(cpu.cpu_usage());
            let usage = format!("{:.2}", cpu.cpu_usage());

            println!("\x1B[2KCore {}: {} | {}%", i, bar, usage);
        }

        println!("-----------------------------");
        
        let used_memory = sys.used_memory() as f32;
        let total_memory = sys.total_memory() as f32;
        let mem_percentage = (used_memory / total_memory) * 100.0;

        let bar = make_bar(mem_percentage);
        println!("\x1B[2KMem: {}", bar);

        print!("\x1B[{}A", num_cpus + 2);
        io::stdout().flush().unwrap();

        thread::sleep(delay);
    }
}

