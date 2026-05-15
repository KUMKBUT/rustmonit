use sysinfo::{
    System,
};
use std::{io::{self, Write}, thread, time::Duration};
use tonic::Request;

pub mod monitor {
    tonic::include_proto!("monitor");
}

use monitor::metrics_collector_client::MetricsCollectorClient;
use monitor::MetricsRequest;

fn make_bar(process: f32) -> String {
    let mut progress_bar = String::from("[....................]");

    let filted_len = (process / 5.0).round() as usize;
    let filted_len = filted_len.min(20);

    if filted_len > 0 {
        progress_bar.replace_range(1..1 + filted_len, &"#".repeat(filted_len));
    }

    progress_bar
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut sys = System::new_all();
    let delay = Duration::from_secs(1);

    let mut client = MetricsCollectorClient::connect("http://100.124.135.86:50051").await?;

    loop {
        sys.refresh_all();

        let cpus = sys.cpus();
        let num_cpus = cpus.len();

        let mut core_reports = Vec::new();

        for (i, cpu) in cpus.iter().enumerate() {
            let usage = cpu.cpu_usage();

            core_reports.push(monitor::CoreData {
                core_id: i as u32,
                usage,
            });

            let bar = make_bar(usage);
            
            println!(
                "\x1B[2KCore {:>2}: {} | {:>5.2}% |",
                i, bar, usage
            );
        }

        println!("--------------------------------|--------|");
        
        let used_b = sys.used_memory() as f32;
        let total_b = sys.total_memory() as f32;
        let mem_percentage = (used_b / total_b) * 100.0;
        let used_gb = used_b / 1024.0 / 1024.0 / 1024.0;

        let hostname = System::host_name().unwrap_or_else(|| "unknown-host".to_string());

        let request = Request::new(MetricsRequest {
            hostname,
            cores: core_reports,
            memory_usage: used_gb,
        });

        if let Err(e) = client.send_metrics(request).await {
            println!("Error: {}", e);
        }

        let bar = make_bar(mem_percentage);
        
        println!(
            "\x1B[2KMem:     {} | {:>4.2}Gb |",
            bar, used_gb
        );

        print!("\x1B[{}A", num_cpus + 2);
        io::stdout().flush().unwrap();

        thread::sleep(delay);
    }
}

