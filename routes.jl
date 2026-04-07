using Genie.Router
using .MailroomController

route("/") do
  serve_static_file("welcome.html")
end

route("/hello", MailroomController.hello)

route("/temp4", MailroomController.temp2)


route("/api/v1/rag/pending", RecordsController.search_pending)