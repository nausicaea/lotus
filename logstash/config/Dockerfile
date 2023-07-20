FROM docker.elastic.co/logstash/logstash:8.5.3
HEALTHCHECK CMD curl -s "http://127.0.0.1:{{ api_port }}" | grep "green"
EXPOSE {{ api_port }}/tcp {{ input_port }}/tcp

COPY jvm.options startup.options log4j2.properties logstash.yml pipelines.yml config/
COPY logstash.conf pipeline/
