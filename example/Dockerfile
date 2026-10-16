FROM golang:1.27.2-bookworm AS build-env

WORKDIR /go/src/app
COPY example.go .
RUN go build -o /go/bin/app example.go

FROM chaos-mesh/toda
COPY --from=build-env /go/bin/app /app
COPY --from=build-env /go/bin/app /main-app

ENV GOMAXPROCS=64
CMD ["/app"]
