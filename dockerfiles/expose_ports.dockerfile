FROM alpine:3.11.6

EXPOSE 8080
EXPOSE 9000
EXPOSE 4567
CMD nc -l -p 8080