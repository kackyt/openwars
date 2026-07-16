import { Button, Container, Paper, Stack, Text, Title } from "@mantine/core";
import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

/**
 * React エラーバウンダリー
 * レンダリング時や子コンポーネントでキャッチされなかった例外（Wasmのパニックなどを含む）を
 * 安全にキャッチし、エラー画面を表示します。
 */
export class ErrorBoundary extends Component<Props, State> {
  public state: State = {
    hasError: false,
    error: null,
  };

  public static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  public componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("ErrorBoundary caught an unhandled error:", error, errorInfo);
  }

  private handleReload = () => {
    window.location.reload();
  };

  public render() {
    if (this.state.hasError) {
      return (
        <Container size="sm" style={{ marginTop: "100px" }}>
          <Paper shadow="md" p="xl" radius="md" withBorder>
            <Stack gap="lg">
              <Title order={2} ta="center" c="red">
                予期しないエラーが発生しました
              </Title>
              <Text size="sm" ta="center">
                ゲームエンジンの処理中、または画面描画中に重大なエラーが発生しました。
                以下のエラー詳細を確認し、解決しない場合はゲームを再起動してください。
              </Text>
              {this.state.error && (
                <Paper p="sm" bg="dark" withBorder>
                  <Text size="xs" style={{ fontFamily: "monospace", whiteSpace: "pre-wrap" }}>
                    {this.state.error.toString()}
                  </Text>
                </Paper>
              )}
              <Button size="md" onClick={this.handleReload} color="red">
                ゲームを再起動
              </Button>
            </Stack>
          </Paper>
        </Container>
      );
    }

    return this.props.children;
  }
}
