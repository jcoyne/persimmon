# syntax=docker/dockerfile:1.7
# Build with the privately prepared Cantaloupe v5.0.7 release tree:
# docker buildx build --platform linux/amd64 \
#   --build-context cantaloupe=/path/to/cantaloupe-v5.0.7 \
#   -f tests/benchmark/cantaloupe.Dockerfile --load -t cantaloupe:bench .
FROM --platform=linux/amd64 eclipse-temurin:21-jre-noble
COPY --from=cantaloupe /target/cantaloupe-5.0.7.jar /opt/cantaloupe/cantaloupe.jar
COPY --from=cantaloupe /dist/deps/Linux-x86-64/lib/ /usr/local/lib/
RUN ldconfig
USER 10001
ENV HOME=/tmp \
    JAVA_TOOL_OPTIONS="-Dcantaloupe.config=/etc/cantaloupe.properties -Djava.library.path=/usr/local/lib"
EXPOSE 8182
ENTRYPOINT ["java", "-jar", "/opt/cantaloupe/cantaloupe.jar"]
