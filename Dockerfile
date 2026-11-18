# The claude-jr image: the one binary on top of busybox, about 3.5 MB.
#
#   docker run --rm -it -e ANTHROPIC_API_KEY -v "$PWD:/work" ghcr.io/iwhitebird/claude-jr
#
# busybox gives the agent a shell and the usual tools (sh, grep, find, sed,
# awk, wget). There is no bash, git or compiler in it, so for work that needs
# those, copy the binary into an image that has them:
#
#   COPY --from=ghcr.io/iwhitebird/claude-jr /usr/local/bin/claudejr /usr/local/bin/
#
# Nothing is compiled here. The release workflow builds it from the binaries
# it has just made, one per architecture:
#
#   docker build --build-context bin=DIR .     DIR holds amd64/claudejr and arm64/claudejr

# Runs on the machine doing the build, whatever the image is for.
FROM --platform=$BUILDPLATFORM busybox:stable-musl AS home
RUN mkdir -m 1777 /home/jr

FROM busybox:stable-musl
LABEL org.opencontainers.image.source="https://github.com/IWhitebird/claude-jr" \
      org.opencontainers.image.description="claude-jr: a small, fast coding agent for the terminal, in one binary" \
      org.opencontainers.image.licenses="MIT"
ARG TARGETARCH
COPY --from=bin --chmod=755 ${TARGETARCH}/claudejr /usr/local/bin/claudejr
# A home any user can write to (sessions and the login are kept there), so
# `--user "$(id -u):$(id -g)"` works and files in /work stay yours.
COPY --from=home /home /home
ENV HOME=/home/jr
WORKDIR /work
ENTRYPOINT ["claudejr"]
