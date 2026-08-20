# syntax=docker/dockerfile:1@sha256:ecfaec9ed6d810b56388c508f4121597bfbba70d41a6dfeee4d8cad5f295fc32
FROM docker.elastic.co/logstash/logstash:9.4.2@sha256:532fa8633866e231d14b8e8860488f224f7f354bc63b43b099ddf2a3a87e2845
HEALTHCHECK CMD curl -s "http://127.0.0.1:{{ api_port }}" | grep "green"
EXPOSE {{ api_port }}/tcp {{ input_port }}/tcp
RUN bin/logstash-plugin install logstash-filter-json_encode

COPY jvm.options startup.options log4j2.properties logstash.yml pipelines.yml config/
COPY {{ pipeline_name }} pipeline/
COPY {{ scripts_dir }} scripts/
COPY {{ patterns_dir }} patterns/
