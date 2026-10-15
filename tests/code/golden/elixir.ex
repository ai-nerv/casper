defmodule MyApp.Web do
  alias Phoenix.Controller
  import Plug.Conn
  use MyApp.Web, :controller
  require Logger

  @doc "Process data"
  def process(conn, params) do
    :ok
  end

  defp validate(data) do
    true
  end
end

defmodule MyApp.Helpers do
  def format_name(name) do
    name
  end
end
