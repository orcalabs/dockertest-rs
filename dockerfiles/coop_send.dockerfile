FROM alpine:3.11.6

RUN apk add nmap-ncat

CMD echo "coop send message to container" | ncat $SEND_TO_IP 45456 && echo "send success"
