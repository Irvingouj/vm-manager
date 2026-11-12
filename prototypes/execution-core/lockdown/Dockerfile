# Runtime image for the lockdown check. The binary is built on the host
# (lockdown.sh) with RUNTIME_SNAPSHOT_PATH=/app/runtime-snapshot.bin.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY ec-host /app/ec-host
COPY hello.js /app/hello.js
COPY runtime-snapshot.bin /app/runtime-snapshot.bin
USER 65532:65532
EXPOSE 8787
ENTRYPOINT ["/app/ec-host", "serve", "/app/hello.js", "0.0.0.0:8787"]
