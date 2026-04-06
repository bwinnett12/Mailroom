
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- 2. Define a custom Type for your status. 
-- This is much more efficient than strings for filtering thousands of records.
CREATE TYPE index_status AS ENUM (
    'down_priority', 
    'down_basic', 
    'down_petty', 
    'stable', 
    'stable_with_errors', 
    'ai_needs_review', 
    'ai_errors', 
    'not_implemented',
    'not_implemented_priority'
);

CREATE TABLE record_index (

	-- Use UUIDs to prevent ID collisions if you sync between Database and other drives
	------ Generated Key
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),



	------ Description of each index
    description TEXT DEFAULT 'NOT-IMPLEMENTED',  -- Loaded via an AI



	------ Index number
	-- -- 82.2 is the unclassified section
    id_index TEXT NOT NULL DEFAULT "82.2-A", -- 


    ------  ------  ------  ------  ------  ------  ------  
	------  ------  -- Status of the Readme of the Index:
	------  -- Options
    ------ Down or not-running
	-- -- Down: Priority -- Fix this index sooner rather than later - Top priority
	-- -- Down: Basic -- Standard error to fix - 2nd highest priority
	-- -- Down: Petty -- Prioritize when there's extra time - Last priority

    ------ Stable or running
	-- -- Stable  -- Good to go 
    -- -- Stable: With Errors  -- Errors with implementation but still running 

    ------ AI implementation
	--------- Reviews the Records for errors, typos, faulty logic, then creates a AI friendly file ("RAG")
	-- -- AI: Needs review -- AI Has not reviewed
	-- -- AI: Errors

    ------ Not implemented
	-- -- Not yet Implemented.   --  Indexes that have not been implemented yet
	-- -- Not yet Implemented: Priority  -- Indexes that have not been made but are a priority to do so

    status index_status DEFAULT 'not_implemented',


	
	------ -- Metadata 
	-- I'm less familiar with this but I know that it will be helpful for RAG, PostgreSQL, and scraping useful information for an AI
	metadata JSONB DEFAULT '{}',  -- Store specific tags and weird data elements
	


	------ -- Time stamp at creation time
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
	------ -- Time stamp at last update time
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,



	------ -- Index path
	------ All Indexes are in one repo... But it speeds things up
	source path TEXT DEFAULT '' -- Replace this with the best format for a string

);

CREATE INDEX idx_inbox_metadata ON record_index USING GIN (metadata);
CREATE INDEX idx_inbox_processed ON record_index (processed) WHERE processed IS FALSE;