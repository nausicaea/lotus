FROM docker.elastic.co/logstash/logstash:8.5.3
HEALTHCHECK CMD test $(curl -s "http://127.0.0.1:9600" | grep -Po '(?<="status":")\w+(?=")') = "green"
EXPOSE 9600/tcp
EXPOSE {{ input_port }}/tcp
EXPOSE {{ output_port }}/tcp

COPY jvm.options config/jvm.options
COPY startup.options config/startup.options
COPY log4j2.properties config/log4j2.properties
COPY logstash.yml config/logstash.yml
COPY logstash.conf pipeline/logstash.conf
