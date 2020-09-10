example-image:
	docker build -t io-example ./example

volume:
	docker volume create io-example

example: example-image volume
	docker run --ulimit nofile=5000:5000 -v io-example:/var/run/test -v /tmp:/tmp -it io-example /main-app

example-inject:debug-toda
	cat ./io-inject-example.json|sudo -E ./target/debug/toda --path /var/run/test --pid $$(pgrep main-app) --verbose trace

image-toda:
	DOCKER_BUILDKIT=1 docker build --build-arg HTTP_PROXY=${HTTP_PROXY} --build-arg HTTPS_PROXY=${HTTPS_PROXY} . -t chaos-mesh/toda 

debug-toda:
	RUSTFLAGS="-Z relro-level=full" cargo build
