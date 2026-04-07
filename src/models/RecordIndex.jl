using SearchLight, SearchLightPostgreSQL
import SearchLight.DbId

# Use Symbols as keys for better performance in Julia
const DEFAULT_RAG_METADATA = Dict{Symbol, Any}(
    :rag => Dict(
        :status => "unprocessed",
        :summary => nothing,
        :version => 1
    ),
    :entities => Dict(
        :tags => String[],
        :detected_logic => false
    ),
    :context => Dict(
        :source_reliability => 0.5
    )
)

@sql_entity mutable struct RecordIndex <: AbstractModel
    id::DbId = DbId()
    description::String = ""
    id_index::String = ""  # Let the DB trigger fill this
    status::String = "NOT-IMPLEMENTED"
    metadata::Dict{Symbol, Any} = deepcopy(DEFAULT_RAG_METADATA)
    source_path::String = ""
    created_at::DateTime = now()
    updated_at::DateTime = now()
end