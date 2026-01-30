#!/usr/bin/env bash
set -euo pipefail

ES_URL=${ES_URL:-http://localhost:9200}

curl -sS -X PUT "$ES_URL/books" -H 'Content-Type: application/json' -d '{
  "settings": { "number_of_shards": 1, "number_of_replicas": 0 }
}'

curl -sS -X POST "$ES_URL/books/_doc/1" -H 'Content-Type: application/json' -d '{"title":"The Pragmatic Programmer","author":"Hunt & Thomas","year":1999}'
curl -sS -X POST "$ES_URL/books/_doc/2" -H 'Content-Type: application/json' -d '{"title":"Clean Code","author":"Robert C. Martin","year":2008}'
curl -sS -X POST "$ES_URL/books/_doc/3" -H 'Content-Type: application/json' -d '{"title":"Designing Data-Intensive Applications","author":"Martin Kleppmann","year":2017}'

curl -sS -X PUT "$ES_URL/users" -H 'Content-Type: application/json' -d '{
  "settings": { "number_of_shards": 1, "number_of_replicas": 0 }
}'

curl -sS -X POST "$ES_URL/users/_doc/1" -H 'Content-Type: application/json' -d '{"name":"alice","role":"admin"}'
curl -sS -X POST "$ES_URL/users/_doc/2" -H 'Content-Type: application/json' -d '{"name":"bob","role":"viewer"}'

curl -sS -X POST "$ES_URL/_refresh" > /dev/null

echo "Seeded indices: books, users"
