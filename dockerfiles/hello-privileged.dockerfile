FROM alpine:3.11.6

CMD [ "/bin/sh", "-c",  "ip link add dummy0 type dummy && echo 'hello dockertest-rs'" ]
