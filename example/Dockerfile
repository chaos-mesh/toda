FROM golang:1.12 as build-env

WORKDIR /go/src/app
ADD . /go/src/app

RUN go get -d -v ./...

RUN go build -o /go/bin/app

FROM chaos-mesh/toda
COPY --from=build-env /go/bin/app /
COPY --from=build-env /go/bin/app /main-app

ENV GOMAXPROCS 64
CMD ["/app"]
