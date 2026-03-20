# reset pekobot config data
rm -Recurse -Force ~/.pekobot

# reset pekobot data for Windows
rm -Recurse -Force ~/AppData/Roaming/pekobot

# set kimi api key
pekobot auth set kimi $env:KIMI_API_KEY

# create an agent with kimi provider
pekobot agent create testagent --provider kimi

# list agents
pekobot agent list

# send a message to the agent
pekobot send testagent "what's USA's capital"

# send a follow-up message to the agent
pekobot send testagent "what about France"

# send a message to the agent with --new flag to start a new session
pekobot send testagent "what about the UK" --new

# list sessions for the agent
pekobot session list testagent