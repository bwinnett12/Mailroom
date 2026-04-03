module MailroomController

using Genie.Renderer.Html, Dates

function hello()
  # This is the 'Logic' part of MVC
  current_time = Dates.format(now(), "HH:MM:SS")
  
  # This returns the 'View' part
  html("<h1>Welcome to Mailroom</h1><p>The time is $current_time</p>")
end

function temp2()
  # Data we want to send to the user
  vars = Dict(
    :user_name => "tarobutter",
    :status    => "System Online",
    :server    => "Island (NixOS)"
  )
  
  # Render the 'temp3.jl.html' file and pass the variables
  ## Loads app/resources/mailroom/views/temp3.html.jl
  html(:mailroom, "temp3", layout=:app, context=@__MODULE__, vars...)
end

end
