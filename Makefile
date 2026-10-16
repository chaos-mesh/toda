example-image: image
	docker build --platform linux/amd64 -t io-example ./example

volume:
	docker volume create io-example

example: example-image volume
	docker run --platform linux/amd64 --privileged --ulimit nofile=5000:5000 -v io-example:/var/run/test -v /tmp:/tmp -it io-example /main-app

debug:
	cargo build --locked

image:
	DOCKER_BUILDKIT=1 docker build --platform linux/amd64 --build-arg HTTP_PROXY=${HTTP_PROXY} --build-arg HTTPS_PROXY=${HTTPS_PROXY} . -t chaos-mesh/toda

release: image
	docker run --platform linux/amd64 -v ${PWD}:/opt/mount:z --rm --entrypoint cp chaos-mesh/toda /toda /opt/mount/toda
