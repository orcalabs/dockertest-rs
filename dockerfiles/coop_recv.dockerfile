FROM alpine:3.11.6

CMD echo "recv started" && nc -lv -p 45456
