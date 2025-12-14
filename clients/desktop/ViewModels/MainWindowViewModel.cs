using System;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Aura.Desktop.Services;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Aura.Desktop.ViewModels;

/// <summary>
/// Main ViewModel for the Aura desktop client.
/// Handles connection, authentication, and voice streaming.
/// </summary>
public partial class MainWindowViewModel : ObservableObject, IAsyncDisposable
{
    private AuraNetworkClient? _client;
    private UserIdentity? _identity;
    private MicrophoneCapture? _mic;
    private CancellationTokenSource? _audioCts;
    
    // ==========================================================================
    // Observable Properties
    // ==========================================================================
    
    [ObservableProperty]
    private bool _isConnected;
    
    [ObservableProperty]
    private bool _isAuthenticated;
    
    [ObservableProperty]
    private string _connectionStatus = "Disconnected";
    
    [ObservableProperty]
    private string _serverAddress = "127.0.0.1";
    
    [ObservableProperty]
    private int _serverPort = 8443;
    
    [ObservableProperty]
    private string _serverPassword = "";
    
    [ObservableProperty]
    private string _displayName = "";
    
    [ObservableProperty]
    private string _publicKeyDisplay = "";
    
    [ObservableProperty]
    private bool _isMicEnabled;
    
    [ObservableProperty]
    private string _audioStats = "";
    
    [ObservableProperty]
    private Channel? _selectedChannel;
    
    [ObservableProperty]
    private ObservableCollection<ChatMessage> _messages = new();
    
    [ObservableProperty]
    private ObservableCollection<Channel> _channels = new();
    
    [ObservableProperty]
    private string _messageInput = "";
    
    // ==========================================================================
    // Initialization
    // ==========================================================================
    
    public MainWindowViewModel()
    {
        // Try to load libmsquic explicitly on macOS
        MsQuicLoader.TryLoadMsQuic();
        
        // Check QUIC support
        QuicSupport.CheckQuicSupport();
        
        // Try to load existing identity
        var identityPath = UserIdentity.GetIdentityFilePath();
        try
        {
            if (System.IO.File.Exists(identityPath))
            {
                _identity = UserIdentity.Load(identityPath);
                DisplayName = _identity.DisplayName;
                PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
                ConnectionStatus = "Ready (identity loaded)";
            }
            else
            {
                ConnectionStatus = "Ready (no identity - will generate on connect)";
            }
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Error loading identity: {ex.Message}";
        }
        
        // Initialize with default channel
        Channels = new ObservableCollection<Channel>
        {
            new Channel 
            { 
                Id = "1", 
                Name = "General", 
                IsExpanded = true,
                Users = new ObservableCollection<User>()
            }
        };
    }
    
    // ==========================================================================
    // Commands
    // ==========================================================================
    
    [RelayCommand]
    private async Task ConnectAsync()
    {
        if (string.IsNullOrWhiteSpace(DisplayName))
        {
            ConnectionStatus = "Error: Display name required";
            return;
        }
        
        try
        {
            // 1. Generate or load identity
            _identity ??= UserIdentity.LoadOrCreate(DisplayName);
            _identity.DisplayName = DisplayName;
            PublicKeyDisplay = _identity.PublicKeyHex[..16] + "...";
            
            // 2. Create client and connect
            _client = new AuraNetworkClient();
            _client.OnStatusChanged += status => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = status);
            _client.OnError += error => 
                Dispatcher.UIThread.Post(() => ConnectionStatus = $"Error: {error}");
            
            ConnectionStatus = "Connecting...";
            await _client.ConnectAsync(ServerAddress, ServerPort);
            IsConnected = true;
            
            // 3. Authenticate with TOFU
            ConnectionStatus = "Authenticating...";
            var password = string.IsNullOrWhiteSpace(ServerPassword) ? null : ServerPassword;
            await _client.AuthenticateAsync(_identity, password);
            IsAuthenticated = true;
            
            ConnectionStatus = $"Connected as {DisplayName} (ID: {_client.UserId})";
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Connected to {ServerAddress}:{ServerPort}",
                System = true 
            });
        }
        catch (AuthenticationException ex)
        {
            ConnectionStatus = $"Auth failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Connection failed: {ex.Message}";
            await DisconnectInternalAsync();
        }
    }
    
    [RelayCommand]
    private async Task DisconnectAsync()
    {
        await DisconnectInternalAsync();
        ConnectionStatus = "Disconnected";
        Messages.Add(new ChatMessage 
        { 
            Content = "Disconnected from server",
            System = true 
        });
    }
    
    private async Task DisconnectInternalAsync()
    {
        StopMic();
        
        if (_client != null)
        {
            await _client.DisposeAsync();
            _client = null;
        }
        
        IsConnected = false;
        IsAuthenticated = false;
    }
    
    [RelayCommand]
    private async Task JoinChannelAsync(Channel? channel)
    {
        if (channel == null || _client == null) return;
        
        try
        {
            SelectedChannel = channel;
            channel.IsExpanded = true;
            
            await _client.JoinChannelAsync(uint.Parse(channel.Id));
            
            Messages.Add(new ChatMessage 
            { 
                Content = $"Joined channel: {channel.Name}",
                System = true 
            });
        }
        catch (Exception ex)
        {
            ConnectionStatus = $"Failed to join channel: {ex.Message}";
        }
    }
    
    [RelayCommand]
    private void ToggleMicrophone()
    {
        if (IsMicEnabled)
        {
            StartMic();
        }
        else
        {
            StopMic();
        }
    }
    
    private void StartMic()
    {
        if (_mic != null || _client == null) return;
        
        _mic = new MicrophoneCapture();
        _audioCts = new CancellationTokenSource();
        
        _mic.OnAudioData += async data =>
        {
            if (_client != null && IsAuthenticated)
            {
                await _client.SendAudioFrameAsync(data, _audioCts.Token);
            }
        };
        
        _mic.OnError += error =>
            Dispatcher.UIThread.Post(() => ConnectionStatus = $"Mic error: {error}");
        
        _mic.Start();
        
        // Update stats periodically
        _ = Task.Run(async () =>
        {
            while (_mic?.IsRunning == true && !_audioCts!.Token.IsCancellationRequested)
            {
                await Task.Delay(1000);
                Dispatcher.UIThread.Post(() => 
                    AudioStats = $"Audio: {_mic.PacketsSent} packets sent");
            }
        });
        
        Messages.Add(new ChatMessage 
        { 
            Content = "Microphone enabled - streaming audio",
            System = true 
        });
    }
    
    private void StopMic()
    {
        _audioCts?.Cancel();
        _mic?.Dispose();
        _mic = null;
        _audioCts = null;
        AudioStats = "";
    }
    
    [RelayCommand]
    private void SendMessage()
    {
        if (string.IsNullOrWhiteSpace(MessageInput)) return;
        
        Messages.Add(new ChatMessage 
        { 
            UserId = _client?.UserId ?? 0,
            UserName = "You",
            Content = MessageInput,
            IsFromCurrentUser = true
        });
        
        // TODO: Send via client
        MessageInput = "";
    }
    
    public async ValueTask DisposeAsync()
    {
        await DisconnectInternalAsync();
        _identity?.Dispose();
        GC.SuppressFinalize(this);
    }
}

// ==========================================================================
// Models
// ==========================================================================

public partial class Channel : ObservableObject
{
    [ObservableProperty] private string _id = "";
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private bool _isExpanded;
    [ObservableProperty] private ObservableCollection<User> _users = new();
}

public partial class User : ObservableObject
{
    [ObservableProperty] private uint _id;
    [ObservableProperty] private string _name = "";
    [ObservableProperty] private string _comment = "";
    [ObservableProperty] private bool _isMuted;
    [ObservableProperty] private bool _isSpeaking;
    [ObservableProperty] private Position3D _position = new(0, 0, 0);
}

public record Position3D(float X, float Y, float Z);

public class ChatMessage
{
    public uint UserId { get; init; }
    public string UserName { get; init; } = "";
    public string Content { get; init; } = "";
    public DateTime Timestamp { get; init; } = DateTime.Now;
    public bool IsFromCurrentUser { get; init; }
    public bool System { get; init; }
}
