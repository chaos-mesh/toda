ARCH ?= amd64

ifneq ($(ARCH),amd64)
ifneq ($(ARCH),arm64)
$(error Unsupported ARCH '$(ARCH)'; use amd64 or arm64)
endif
endif

example-image: image
	docker build --platform linux/$(ARCH) -t io-example ./example

volume:
	docker volume create io-example

example: example-image volume
	docker run --platform linux/$(ARCH) --privileged --ulimit nofile=5000:5000 -v io-example:/var/run/test -v /tmp:/tmp -it io-example /main-app

debug:
	cargo build --locked

image:
	DOCKER_BUILDKIT=1 docker build --platform linux/$(ARCH) --build-arg HTTP_PROXY=${HTTP_PROXY} --build-arg HTTPS_PROXY=${HTTPS_PROXY} . -t chaos-mesh/toda

release: image
	docker run --platform linux/$(ARCH) -v ${PWD}:/opt/mount:z --rm --entrypoint cp chaos-mesh/toda /toda /opt/mount/toda
