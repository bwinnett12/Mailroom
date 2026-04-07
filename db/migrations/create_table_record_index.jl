function up()
    create_table(:record_index) do
        [
            column(:id, :uuid, "PRIMARY KEY DEFAULT uuid_generate_v4()"),
            column(:description, :text, "DEFAULT 'NOT-IMPLEMENTED'"),
            column(:id_index, :text, "NOT NULL"), # Trigger handles the default
            column(:status, :text, "DEFAULT 'NOT-IMPLEMENTED'"),
            column(:metadata, :jsonb, "DEFAULT '{}'"),
            column(:source_path, :text, "DEFAULT ''"),
            column(:created_at, :timestamp, "DEFAULT CURRENT_TIMESTAMP"),
            column(:updated_at, :timestamp, "DEFAULT CURRENT_TIMESTAMP")
        ]
    end
    # Crucial: Add the GIN index for RAG performance
    execute("CREATE INDEX idx_record_metadata_gin ON record_index USING GIN (metadata)")
end