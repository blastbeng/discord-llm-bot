FROM golang:1.24-alpine AS builder
RUN apk add --no-cache gcc musl-dev ffmpeg git
WORKDIR /app
ENV GOPROXY=https://proxy.golang.org,direct

# Copy go.mod and go.sum (if present) first to leverage Docker cache
COPY go.* ./
RUN go mod download

# Copy the rest of the project sources
COPY . .
RUN CGO_ENABLED=1 go build -mod=mod -o /discord-bot .

FROM alpine:latest
RUN apk add --no-cache ffmpeg
WORKDIR /app
COPY --from=builder /discord-bot .
CMD ["./discord-bot"]
