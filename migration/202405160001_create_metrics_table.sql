CREATE TABLE IF NOT EXISTS host_metrics (
    id SERIAL PRIMARY KEY,
    hostname TEXT NOT NULL,
    cpu_usage DOUBLE PRECISION[] NOT NULL, 
    memory_usage DOUBLE PRECISION NOT NULL, 
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_metrics_hostname_time ON host_metrics (hostname, created_at);
