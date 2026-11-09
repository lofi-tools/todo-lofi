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

CREATE TABLE companies (
  company_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  homepage_url VARCHAR,
  source_url VARCHAR NOT NULL
);

CREATE TABLE company_plans (
  plan_id VARCHAR PRIMARY KEY,
  company_id VARCHAR NOT NULL,
  name VARCHAR NOT NULL,
  provider_id VARCHAR,
  source_url VARCHAR NOT NULL
);

CREATE TABLE providers (
  provider_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  npm VARCHAR,
  api_url VARCHAR,
  doc_url VARCHAR,
  env_keys VARCHAR NOT NULL,
  logo_path VARCHAR,
  retained BOOLEAN NOT NULL DEFAULT FALSE,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL
);

CREATE TABLE model_families (
  family_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  parent_family_id VARCHAR,
  vendor VARCHAR
);

CREATE TABLE canonical_models (
  canonical_model_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  lab VARCHAR NOT NULL,
  family_id VARCHAR
);

CREATE TABLE models (
  provider_id VARCHAR NOT NULL,
  model_id VARCHAR NOT NULL,
  model_type VARCHAR,
  canonical_model_id VARCHAR NOT NULL,
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
  retained BOOLEAN NOT NULL DEFAULT FALSE,
  source_id VARCHAR NOT NULL,
  source_url VARCHAR NOT NULL,
  source_record VARCHAR NOT NULL,
  PRIMARY KEY (provider_id, model_id)
);

CREATE TABLE harnesses (
  harness_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  vendor VARCHAR,
  homepage_url VARCHAR
);

CREATE TABLE provider_harnesses (
  provider_id VARCHAR NOT NULL,
  harness_id VARCHAR NOT NULL,
  exclusive BOOLEAN NOT NULL,
  source_url VARCHAR NOT NULL,
  PRIMARY KEY (provider_id, harness_id)
);

CREATE TABLE benchmarks (
  benchmark_id VARCHAR PRIMARY KEY,
  name VARCHAR NOT NULL,
  description VARCHAR,
  unit VARCHAR NOT NULL CHECK (unit IN ('percent', 'score')),
  higher_is_better BOOLEAN NOT NULL,
  source_url VARCHAR NOT NULL
);

CREATE TABLE model_benchmarks (
  canonical_model_id VARCHAR NOT NULL,
  benchmark_id VARCHAR NOT NULL,
  score DECIMAL(18, 6) NOT NULL,
  source_url VARCHAR NOT NULL,
  PRIMARY KEY (canonical_model_id, benchmark_id)
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
  source_record VARCHAR NOT NULL
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
  source_record VARCHAR NOT NULL
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
  PRIMARY KEY (provider_id, model_id, runtime)
);