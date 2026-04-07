module RecordsController
using Genie.Renderer.Json, SearchLight, App.RecordIndex

function search_pending()
  # 1. Grab the reliability threshold from the URL params (e.g., /search?min=0.8)
  threshold = getpayload(:min, 0.5) |> parse_float
  
  # 2. Run the specialized GIN-indexed query
  # We use the JSONB path operator '->>' to extract the status as text
  pending_records = find(RecordIndex, 
    SQLWhereEntity("metadata->'rag'->>'status' = ? AND (metadata->'context'->>'source_reliability')::float >= ?", 
                   ["unprocessed", threshold]))

  # 3. Return as JSON for your frontend or AI agent
  return json(Dict(:records => pending_records))
end

# Helper to safely parse params
parse_float(s::String) = tryparse(Float64, s)
parse_float(n::Real) = Float64(n)

end