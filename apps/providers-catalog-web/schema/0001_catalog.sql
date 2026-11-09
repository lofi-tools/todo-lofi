CREATE TABLE sources (
  source_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  license VARCHAR NOT NULL,
  attribution VARCHAR NOT NULL,
  fetched_at TIMESTAMP NOT NULL,
  http_status INTEGER,
  etag VARCHAR,
  content_sha256 VARCHAR NOT NULL,
  status VARCHAR NOT NULL,
  error VARCHAR
);

CREATE TABLE scrape_runs (
  run_id VARCHAR PRIMARY KEY,
  source_id VARCHAR NOT NULL,
  started_at TIMESTAMP NOT NULL,
  finished_at TIMESTAMP NOT NULL,
  status VARCHAR NOT NULL,
  snapshot_id VARCHAR,
  error VARCHAR
);

CREATE TABLE catalog_meta (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  snapshot_id VARCHAR NOT NULL UNIQUE,
  schema_version INTEGER NOT NULL,
  generated_at TIMESTAMP NOT NULL,
  source_fetched_at TIMESTAMP NOT NULL,
  freshness_deadline TIMESTAMP NOT NULL,
  license VARCHAR NOT NULL,
  attribution VARCHAR NOT NULL,
  source_urls VARCHAR NOT NULL,
  source_hashes VARCHAR NOT NULL,
  known_gaps VARCHAR NOT NULL
);

CREATE TABLE agents (
  agent_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  vendor VARCHAR,
  homepage_url VARCHAR,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR
);

CREATE TABLE providers (
  provider_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  npm VARCHAR,
  api_url VARCHAR,
  doc_url VARCHAR,
  env_keys VARCHAR NOT NULL,
  logo_path VARCHAR,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR
);

CREATE TABLE models (
  provider_id VARCHAR NOT NULL,
  model_id VARCHAR NOT NULL,
  model_type VARCHAR,
  canonical_model_id VARCHAR,
  name VARCHAR,
  family VARCHAR,
  description VARCHAR,
  release_date VARCHAR,
  last_updated VARCHAR,
  knowledge_date VARCHAR,
  open_weights BOOLEAN,
  attachment BOOLEAN,
  reasoning BOOLEAN,
  tool_call BOOLEAN,
  structured_output BOOLEAN,
  temperature BOOLEAN,
  interleaved VARCHAR,
  context_limit BIGINT,
  input_limit BIGINT,
  output_limit BIGINT,
  cost_input DECIMAL(18, 6),
  cost_output DECIMAL(18, 6),
  cost_reasoning DECIMAL(18, 6),
  cost_cache_read DECIMAL(18, 6),
  cost_cache_write DECIMAL(18, 6),
  cost_input_audio DECIMAL(18, 6),
  cost_output_audio DECIMAL(18, 6),
  modalities_input VARCHAR,
  modalities_output VARCHAR,
  status VARCHAR CHECK (status IS NULL OR status IN ('alpha', 'beta', 'deprecated')),
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (provider_id, model_id)
);

CREATE TABLE plans (
  plan_id VARCHAR PRIMARY KEY,
  provider_id VARCHAR NOT NULL,
  name VARCHAR NOT NULL,
  usage_model VARCHAR NOT NULL CHECK (usage_model IN ('subscription', 'token_pack', 'pay_as_you_go', 'free', 'unknown')),
  billing_period VARCHAR,
  price DECIMAL(18, 6),
  currency VARCHAR,
  included_allowance DECIMAL(18, 6),
  allowance_unit VARCHAR,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR
);

CREATE TABLE free_offers (
  offer_id VARCHAR PRIMARY KEY,
  provider_id VARCHAR NOT NULL,
  plan_id VARCHAR,
  offer_kind VARCHAR NOT NULL CHECK (offer_kind = 'recurring'),
  allowance DECIMAL(18, 6) NOT NULL,
  allowance_unit VARCHAR NOT NULL,
  reset_period VARCHAR NOT NULL,
  eligibility VARCHAR,
  conditions VARCHAR,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR
);

CREATE TABLE agent_provider_links (
  agent_id VARCHAR NOT NULL,
  provider_id VARCHAR NOT NULL,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (agent_id, provider_id)
);

CREATE TABLE agent_plan_links (
  agent_id VARCHAR NOT NULL,
  plan_id VARCHAR NOT NULL,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (agent_id, plan_id)
);

CREATE TABLE agent_model_links (
  agent_id VARCHAR NOT NULL,
  provider_id VARCHAR NOT NULL,
  model_id VARCHAR NOT NULL,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (agent_id, provider_id, model_id)
);

CREATE TABLE local_runtimes (
  provider_id VARCHAR NOT NULL,
  model_id VARCHAR NOT NULL,
  runtime VARCHAR NOT NULL,
  quantization VARCHAR,
  approximate_hardware VARCHAR,
  setup_notes VARCHAR,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (provider_id, model_id, runtime)
);

CREATE TABLE fact_sources (
  fact_type VARCHAR NOT NULL,
  fact_id VARCHAR NOT NULL,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  verified_at TIMESTAMP NOT NULL,
  last_attempt_at TIMESTAMP NOT NULL,
  confidence VARCHAR NOT NULL CHECK (confidence IN ('verified', 'community', 'inferred', 'unknown')),
  scrape_status VARCHAR NOT NULL CHECK (scrape_status IN ('ok', 'missing', 'failed', 'not_fetched')),
  last_error VARCHAR,
  PRIMARY KEY (fact_type, fact_id, source_id)
);
