use tonic::{transport::Server, Request, Response, Status};

pub mod monitor {
    tonic::include_proto!("monitor");
}

use monitor::metrics_collector_server::{MetricsCollector, MetricsCollectorServer};
use monitor::{MetricsRequest, MetricsResponse};

#[derive(Default)]
pub struct MyCollector {}

#[tonic::async_trait]
impl MetricsCollector for MyCollector {
    async fn send_metrics(
        &self,
        request: Request<MetricsRequest>,
    ) -> Result<Response<MetricsResponse>, Status> {
        let req = request.into_inner();

        println!("Received metrics: Core {} -> {}%", req.core_id, req.usage);

        let reply = MetricsResponse {
            success: true,
        };

        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let collector = MyCollector::default();

    println!("Server listening on {}", addr);

    Server::builder()
        .add_service(MetricsCollectorServer::new(collector))
        .serve(addr)
        .await?;

    Ok(())
}
