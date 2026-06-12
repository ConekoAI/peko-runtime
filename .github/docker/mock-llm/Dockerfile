FROM python:3.12-slim
WORKDIR /app
RUN pip install --no-cache-dir fastapi uvicorn
COPY mock_llm_server.py .
EXPOSE 8080
CMD ["python", "mock_llm_server.py", "--host", "0.0.0.0", "--port", "8080"]
