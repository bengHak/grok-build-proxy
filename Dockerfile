FROM golang:1.23-alpine AS build
WORKDIR /src
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags "-s -w" -o /out/grok-build-proxy ./cmd/grok-build-proxy

FROM alpine:3.22
RUN apk add --no-cache ca-certificates && adduser -D -u 10001 proxy
USER proxy
COPY --from=build /out/grok-build-proxy /usr/local/bin/grok-build-proxy
EXPOSE 18765
ENTRYPOINT ["grok-build-proxy"]
