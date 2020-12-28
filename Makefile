example-image: image
	docker build -t io-example ./example

volume:
	docker volume create io-example

example: example-image volume
	docker run --privileged --ulimit nofile=5000:5000 -v io-example:/var/run/test -v /tmp:/tmp -it io-example /main-app

debug:
	cargo build

image:
	DOCKER_BUILDKIT=1 docker build --build-arg HTTP_PROXY=${HTTP_PROXY} --build-arg HTTPS_PROXY=${HTTPS_PROXY} . -t chaos-mesh/toda

release: image
	docker run -v ${PWD}:/opt/mount:z --rm --entrypoint cp chaos-mesh/toda /toda /opt/mount/toda
