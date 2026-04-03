module MailroomController

using Genie.Renderer.Html, Dates

function hello()
  # This is the 'Logic' part of MVC
  current_time = Dates.format(now(), "HH:MM:SS")
  
  # This returns the 'View' part
  html("<h1>Welcome to Mailroom</h1><p>The time is $current_time</p>")
end

end