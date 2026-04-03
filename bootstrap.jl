using Pkg
(pwd() != @__DIR__) && cd(@__DIR__)
Pkg.activate(".")

# Load your local files
include("MailroomController.jl")
include("routes.jl")

using Genie
# Start the server (sync=false for systemd, true for REPL)
Genie.up(5150, "0.0.0.0", async=false)